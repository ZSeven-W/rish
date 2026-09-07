//! Bounded system-resolver fallback for mobile hosts without resolv.conf.
//! Two process-wide workers call the host's getaddrinfo through std. No
//! interpreter thread blocks and resetting a VM cannot deliver an old reply.

use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

const CAPACITY: usize = 64;
const TIMEOUT: Duration = Duration::from_secs(30);

struct Job {
    id: u64,
    name: String,
    deadline: Instant,
    reply: mpsc::SyncSender<Resolved>,
}

struct Resolved {
    id: u64,
    addresses: Result<Vec<IpAddr>, ()>,
}

fn workers() -> Option<&'static mpsc::SyncSender<Job>> {
    static WORKERS: OnceLock<Option<mpsc::SyncSender<Job>>> = OnceLock::new();
    WORKERS
        .get_or_init(|| {
            let (send, receive) = mpsc::sync_channel::<Job>(CAPACITY);
            let receive = Arc::new(Mutex::new(receive));
            let mut count = 0;
            for index in 0..2 {
                let receive = Arc::clone(&receive);
                if std::thread::Builder::new()
                    .name(format!("rish-system-dns-{index}"))
                    .spawn(move || {
                        loop {
                            let job = match receive.lock() {
                                Ok(receiver) => receiver.recv(),
                                Err(_) => return,
                            };
                            let Ok(job) = job else { return };
                            if Instant::now() >= job.deadline {
                                continue;
                            }
                            let addresses = (job.name.as_str(), 0)
                                .to_socket_addrs()
                                .map(|addresses| {
                                    addresses.take(32).map(|address| address.ip()).collect()
                                })
                                .map_err(|_| ());
                            let _ = job.reply.try_send(Resolved {
                                id: job.id,
                                addresses,
                            });
                        }
                    })
                    .is_ok()
                {
                    count += 1;
                }
            }
            (count > 0).then_some(send)
        })
        .as_ref()
}

struct Question {
    wire: Vec<u8>,
    name: String,
    kind: u16,
}

impl Question {
    fn parse(packet: &[u8]) -> Option<Self> {
        if packet.len() < 12
            || packet.len() > 4096
            || packet[2] & 0xfe != 0
            || packet[4..6] != [0, 1]
            || packet[6..10] != [0; 4]
        {
            return None;
        }
        let mut offset = 12;
        let mut labels = Vec::new();
        loop {
            let length = usize::from(*packet.get(offset)?);
            offset += 1;
            if length == 0 {
                break;
            }
            if length > 63 {
                return None;
            }
            let label = packet.get(offset..offset.checked_add(length)?)?;
            if !label
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return None;
            }
            labels.push(std::str::from_utf8(label).ok()?);
            offset += length;
            if offset > 267 {
                return None;
            }
        }
        let trailer = packet.get(offset..offset.checked_add(4)?)?;
        if trailer[2..] != [0, 1] {
            return None;
        }
        let name = labels.join(".");
        if name.is_empty() || name.len() > 253 {
            return None;
        }
        Some(Self {
            wire: packet[..offset + 4].to_vec(),
            name,
            kind: u16::from_be_bytes([trailer[0], trailer[1]]),
        })
    }

    fn answer(&self, addresses: Result<&[IpAddr], ()>) -> Vec<u8> {
        let mut answer = self.wire.clone();
        let unsupported = !matches!(self.kind, 1 | 28);
        let rcode = if unsupported {
            4
        } else if addresses.is_err() {
            2
        } else {
            0
        };
        answer[2] = 0x80 | (self.wire[2] & 1); // QR and copied RD
        answer[3] = 0x80 | rcode; // RA; no claim of DNSSEC authentication
        answer[6..12].fill(0);
        let mut count = 0_u16;
        let mut seen = Vec::new();
        if !unsupported {
            for address in addresses.unwrap_or(&[]) {
                if seen.contains(address) {
                    continue;
                }
                let bytes = match (self.kind, address) {
                    (1, IpAddr::V4(ip)) => ip.octets().to_vec(),
                    (28, IpAddr::V6(ip)) => ip.octets().to_vec(),
                    _ => continue,
                };
                if answer.len() + 12 + bytes.len() > 512 {
                    answer[2] |= 2; // TC; never return a partial record
                    break;
                }
                answer.extend_from_slice(&[0xc0, 0x0c]);
                answer.extend_from_slice(&self.kind.to_be_bytes());
                answer.extend_from_slice(&[0, 1, 0, 0, 0, 0]); // IN, TTL 0 (OS does not expose TTL)
                answer.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                answer.extend_from_slice(&bytes);
                seen.push(*address);
                count += 1;
            }
        }
        answer[6..8].copy_from_slice(&count.to_be_bytes());
        answer
    }
}

struct Pending {
    port: u16,
    question: Question,
    deadline: Instant,
}

pub(super) struct SystemDns {
    send: mpsc::SyncSender<Resolved>,
    receive: mpsc::Receiver<Resolved>,
    pending: HashMap<u64, Pending>,
    next_id: u64,
}

impl SystemDns {
    pub fn new() -> Self {
        let (send, receive) = mpsc::sync_channel(CAPACITY);
        Self {
            send,
            receive,
            pending: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn submit(&mut self, packet: &[u8], port: u16) -> bool {
        let Some(question) = Question::parse(packet) else {
            return false;
        };
        if self.pending.len() >= CAPACITY {
            return false;
        }
        let id = self.next_id;
        let Some(next) = id.checked_add(1) else {
            return false;
        };
        self.next_id = next;
        let deadline = Instant::now() + TIMEOUT;
        if matches!(question.kind, 1 | 28) {
            let Some(queue) = workers() else { return false };
            if queue
                .try_send(Job {
                    id,
                    name: question.name.clone(),
                    deadline,
                    reply: self.send.clone(),
                })
                .is_err()
            {
                return false;
            }
        } else if self
            .send
            .try_send(Resolved {
                id,
                addresses: Err(()),
            })
            .is_err()
        {
            return false;
        }
        self.pending.insert(
            id,
            Pending {
                port,
                question,
                deadline,
            },
        );
        true
    }

    pub fn poll(&mut self, now: Instant) -> Vec<(u16, Vec<u8>)> {
        let mut answers = Vec::new();
        while let Ok(resolved) = self.receive.try_recv() {
            if let Some(pending) = self.pending.remove(&resolved.id) {
                let addresses = if now < pending.deadline {
                    resolved.addresses.as_deref().map_err(|_| ())
                } else {
                    Err(())
                };
                answers.push((pending.port, pending.question.answer(addresses)));
            }
        }
        self.pending.retain(|_, pending| {
            if now < pending.deadline {
                true
            } else {
                answers.push((pending.port, pending.question.answer(Err(()))));
                false
            }
        });
        answers
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        // Do not rewind IDs: a worker from the old session may still finish.
        while self.receive.try_recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(kind: u16) -> Vec<u8> {
        let mut query = vec![0xbe, 0xef, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 9];
        query.extend_from_slice(b"localhost");
        query.push(0);
        query.extend_from_slice(&kind.to_be_bytes());
        query.extend_from_slice(&[0, 1]);
        query
    }

    #[test]
    fn answers_preserve_question_and_select_address_family() {
        let addresses = ["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()];
        for (kind, length) in [(1, 4), (28, 16)] {
            let request = query(kind);
            let question = Question::parse(&request).unwrap();
            let answer = question.answer(Ok(&addresses));
            assert_eq!(&answer[..2], &[0xbe, 0xef]);
            assert_eq!(&answer[6..8], &[0, 1]);
            assert_eq!(&answer[12..request.len()], &request[12..]);
            assert_eq!(answer.len(), request.len() + 12 + length);
            assert_eq!(&answer[request.len() + 6..request.len() + 10], &[0; 4]);
        }
    }

    #[test]
    fn malformed_names_and_multiple_questions_are_rejected() {
        let mut compressed = query(1);
        compressed[12] = 0xc0;
        assert!(Question::parse(&compressed).is_none());
        assert!(Question::parse(&query(1)[..14]).is_none());
        assert!(Question::parse(&[]).is_none());
        let mut request = query(1);
        request[5] = 2;
        assert!(Question::parse(&request).is_none());
        let mut request = query(1);
        request[13] = b'.';
        assert!(Question::parse(&request).is_none());
    }

    #[test]
    fn errors_are_not_cached_as_nxdomain_and_unsupported_types_are_explicit() {
        assert_eq!(
            Question::parse(&query(1)).unwrap().answer(Err(()))[3] & 15,
            2
        );
        assert_eq!(
            Question::parse(&query(16)).unwrap().answer(Ok(&[]))[3] & 15,
            4
        );
    }

    #[test]
    fn reset_rejects_late_results_even_when_guest_ids_are_reused() {
        let mut resolver = SystemDns::new();
        resolver.pending.insert(
            0,
            Pending {
                port: 1000,
                question: Question::parse(&query(1)).unwrap(),
                deadline: Instant::now() + TIMEOUT,
            },
        );
        resolver.next_id = 1;
        resolver.reset();
        assert_eq!(resolver.next_id, 1);
        resolver.pending.insert(
            1,
            Pending {
                port: 2000,
                question: Question::parse(&query(1)).unwrap(),
                deadline: Instant::now() + TIMEOUT,
            },
        );
        resolver
            .send
            .send(Resolved {
                id: 0,
                addresses: Ok(vec!["127.0.0.1".parse().unwrap()]),
            })
            .unwrap();
        assert!(resolver.poll(Instant::now()).is_empty());
        assert!(resolver.pending.contains_key(&1));
        resolver
            .send
            .send(Resolved {
                id: 1,
                addresses: Ok(vec!["127.0.0.1".parse().unwrap()]),
            })
            .unwrap();
        let answers = resolver.poll(Instant::now());
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].0, 2000);
    }

    #[test]
    fn pending_capacity_and_expiry_are_bounded() {
        let mut resolver = SystemDns::new();
        let now = Instant::now();
        for id in 0..CAPACITY as u64 {
            resolver.pending.insert(
                id,
                Pending {
                    port: 2000,
                    question: Question::parse(&query(1)).unwrap(),
                    deadline: now + TIMEOUT,
                },
            );
        }
        assert!(!resolver.submit(&query(16), 3000));
        let answers = resolver.poll(now + TIMEOUT);
        assert_eq!(answers.len(), CAPACITY);
        assert!(answers.iter().all(|(_, packet)| packet[3] & 15 == 2));
        assert!(resolver.pending.is_empty());
    }
}
