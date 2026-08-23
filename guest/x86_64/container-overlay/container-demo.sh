#!/bin/busybox sh
# Launches a throwaway container from inside the guest: a fresh root populated
# with busybox, entered through unshare (new pid/mount/uts/ipc/net namespaces)
# plus chroot. Demonstrates namespace isolation without a container runtime.
mkdir -p /c/bin /c/lib /c/dev /c/proc
cp /bin/busybox /c/bin/
cp /lib/ld-musl-x86_64.so.1 /c/lib/
mknod /c/dev/null c 1 3 2>/dev/null
mknod /c/dev/console c 5 1 2>/dev/null
cat > /c/greet.sh <<'G'
#!/bin/busybox sh
/bin/busybox hostname rish-container 2>/dev/null
echo "hello from inside a container on iOS"
echo "I am PID $$ in an isolated PID namespace"
echo "container hostname: $(/bin/busybox hostname)"
/bin/busybox mount -t proc proc /proc 2>/dev/null
echo "container sees $(/bin/busybox ps 2>/dev/null | /bin/busybox wc -l) process(es)"
/bin/busybox uname -a
G
chmod 0755 /c/greet.sh
echo "launching a container: unshare pid/mount/uts/ipc/net + chroot"
unshare --pid --mount --uts --ipc --net --fork chroot /c /bin/busybox sh /greet.sh
echo "container exited with code $?"
