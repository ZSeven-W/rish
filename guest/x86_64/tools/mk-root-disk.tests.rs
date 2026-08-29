    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn test_dir(label: &str) -> PathBuf {
        let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "rish-root-disk-{label}-{}-{counter}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn build_from(overlay: &Path) -> Vec<u8> {
        let mut root = new_node(String::new(), Kind::Directory(Vec::new()));
        let children = match &mut root.kind {
            Kind::Directory(children) => children,
            Kind::File(_) => unreachable!(),
        };
        read_tree(overlay, children).unwrap();
        let mut generic_counter = 0_u32;
        assign_shorts(children, &mut generic_counter);
        build_image(&root).unwrap()
    }

    fn write_tree(paths: &[(&str, &str)]) -> PathBuf {
        let dir = test_dir("tree");
        for (name, content) in paths {
            let target = dir.join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, content).unwrap();
        }
        dir
    }

    #[test]
    fn validate_name_rejects_unsafe_components() {
        for name in ["", ".", "..", "a/b", "a\u{0}b", "bad?name"] {
            assert!(validate_name(name).is_err(), "{name:?} should be rejected");
        }
        assert!(validate_name(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(validate_name("etc/apk/repositories".rsplit('/').next().unwrap()).is_ok());
    }

    #[test]
    fn short_names_are_stable() {
        assert_eq!(canonical_83(&short_candidate("APK").unwrap()), "APK");
        assert_eq!(canonical_83(&short_candidate("PIP.CON").unwrap()), "PIP.CON");
        assert_eq!(
            canonical_83(&short_candidate("etc").unwrap()),
            "ETC",
            "lowercase still shortens"
        );
        assert!(short_candidate("repositories").is_none(), "too long for 8.3");
        assert!(short_candidate(".npmrc").is_none(), "dotfiles get a generic name");
        assert!(short_candidate("a.b.c").is_none(), "extra dots get a generic name");
        assert_eq!(canonical_83(&generic_short(7)), "RISH0007");
    }

    #[test]
    fn lfn_chunks_round_trip() {
        for name in ["repositories", ".npmrc", "a", "1234567890123456789012345678901234567890"] {
            let chunks = lfn_chunks(name);
            let utf16: Vec<u16> = name.encode_utf16().collect();
            let mut joined: Vec<u16> = Vec::new();
            for chunk in &chunks {
                joined.extend(chunk.iter().take_while(|ch| **ch != 0x0000));
            }
            assert_eq!(joined, utf16, "chunks of {name:?} join back");
        }
        let checksum = |short: [u8; 11]| lfn_checksum(&short);
        let one = checksum(*b"ETC        ");
        assert_eq!(checksum(*b"ETC        "), one, "checksum is deterministic");
    }

    #[test]
    fn lfn_chunks_pad_with_a_nul_terminator_then_ffff_filler() {
        // The convention the Linux fat driver itself writes for its own
        // entries: name, one 0x0000 terminator, then 0xFFFF filler. Plain
        // 0xFFFF padding would render as '?' in every long name on Linux.
        let chunks = lfn_chunks("xt");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0][0], u16::from(b'x'));
        assert_eq!(chunks[0][1], u16::from(b't'));
        assert_eq!(chunks[0][2], 0x0000);
        assert!(chunks[0][3..].iter().all(|ch| *ch == 0xFFFF));
    }

    #[test]
    fn lfn_checksum_matches_the_on_disk_format_definition() {
        // Reference values computed with the canonical algorithm from the
        // VFAT LFN definition (also used by Linux fs/fat/dir.c fat_checksum):
        // sum = ((sum & 1) << 7) | (sum >> 1), then add the next name byte.
        // The previous left-rotating variant produced 0xa1/0xa9 here, which
        // macOS tolerates but Linux rejects (long names were dropped).
        assert_eq!(lfn_checksum(b"RISH0000   "), 0x90);
        assert_eq!(lfn_checksum(b"RISH0001   "), 0x70);
        assert_eq!(lfn_checksum(b"ETC        "), 0xAD);
    }

    #[test]
    fn round_trip_preserves_tree_and_bytes() {
        let overlay = write_tree(&[
            ("etc/apk/repositories", "https://mirrors.example/alpine/v3.24/main
"),
            ("etc/pip.conf", "[global]
index-url = https://mirrors.example/pypi
"),
            (".npmrc", "registry=https://mirrors.example/npm
"),
            ("usr/share/long-directory-name/README.md", "# long names work
"),
            ("empty.txt", ""),
        ]);
        let image = build_from(&overlay);
        let extracted = test_dir("extracted");
        extract_tree(&image, &extracted).unwrap();
        for (name, content) in [
            ("etc/apk/repositories", "https://mirrors.example/alpine/v3.24/main
"),
            ("etc/pip.conf", "[global]
index-url = https://mirrors.example/pypi
"),
            (".npmrc", "registry=https://mirrors.example/npm
"),
            ("usr/share/long-directory-name/README.md", "# long names work
"),
            ("empty.txt", ""),
        ] {
            let bytes = fs::read(extracted.join(name)).unwrap();
            assert_eq!(bytes, content.as_bytes(), "content of {name}");
        }
        fs::remove_dir_all(&overlay).unwrap();
        fs::remove_dir_all(&extracted).unwrap();
    }

    #[test]
    fn identical_inputs_build_identical_images() {
        let overlay = write_tree(&[("etc/apk/repositories", "line one
line two
")]);
        let first = build_from(&overlay);
        let second = build_from(&overlay);
        assert_eq!(first, second, "images must be byte-identical");
        assert_eq!(first.len(), IMAGE_SECTORS as usize * SECTOR_BYTES);
        fs::remove_dir_all(&overlay).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_rejected() {
        let overlay = test_dir("symlink");
        fs::write(overlay.join("good.txt"), "fine
").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", overlay.join("bad.txt")).unwrap();
        let mut root = new_node(String::new(), Kind::Directory(Vec::new()));
        let children = match &mut root.kind {
            Kind::Directory(children) => children,
            Kind::File(_) => unreachable!(),
        };
        let error = read_tree(&overlay, children).unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error}");
        fs::remove_dir_all(&overlay).unwrap();
    }

    #[test]
    fn non_utf8_names_are_rejected_by_the_walk() {
        // The pure-name checks (including the 255-char VFAT limit and the
        // forbidden character set) are covered by
        // validate_name_rejects_unsafe_components; here the walk itself is
        // exercised with a name that is not valid UTF-8. Filesystems may
        // refuse to create such a probe, so skip gracefully in that case.
        let overlay = test_dir("nonutf8");
        use std::os::unix::ffi::OsStrExt;
        let bad = std::ffi::OsStr::from_bytes(&[b'b', b'a', b'd', 0xFF]);
        if fs::write(overlay.join(bad), "x").is_err() {
            eprintln!("filesystem refused the non-UTF8 probe; skipping the walk case");
            fs::remove_dir_all(&overlay).unwrap();
            return;
        }
        let mut root = new_node(String::new(), Kind::Directory(Vec::new()));
        let children = match &mut root.kind {
            Kind::Directory(children) => children,
            Kind::File(_) => unreachable!(),
        };
        assert!(read_tree(&overlay, children).is_err());
        fs::remove_dir_all(&overlay).unwrap();
    }
