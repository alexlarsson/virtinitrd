# virtintrd

A minimal initrd for spawning VMs with virtiofs mounted rootfs.

## Features

- Single statically liked initrd binary
- Supports kernels with virtiofs being modular

## Prerequisites

Install the musl target for static linking:

```bash
rustup target add x86_64-unknown-linux-musl
```

## Building

### Step 1: Build the binary

```bash
cargo build --release
```

The statically linked binary will be at `target/x86_64-unknown-linux-musl/release/virtintrd`.

### Step 2: Build initramfs

```bash
./mkvirtinitrd target/x86_64-unknown-linux-musl/release/virtintrd /usr/lib/modules/$(uname -r) initrd.img
```

## Usage with QEMU

```bash
qemu-system-x86_64 \
    -kernel /boot/vmlinuz-$(uname -r) \
    -initrd initrd.img \
    -append "console=ttyS0" \
    -nographic
```

## Module Management

Use the `--module=` option to include additional kernel modules in the initramfs:

```bash
./mkvirtinitrd \
    --module=virtio_blk \
    --module=ext4 \
    --module=nvme \
    target/x86_64-unknown-linux-musl/release/virtintrd \
    /usr/lib/modules/$(uname -r) \
    initrd.img
```

The virtiofs module is always included automatically.

## Kernel command line options

The initrd handles a few options:

 * `init`: If specified, this is the program run as pid 1, defaults to `/bin/sh`
 * 'debug': If specified, debug output is printed
 * 'rootfs': If specified, this virtiofs tag is used for the rootfs mount, default is `rootfs`
 * `mount=foo`: If specified the virtiofs tag `foo` is mounted read-write at `/run/mnt/foo`
 * `mount-ro=foo`: If specified the virtiofs tag `foo` is mounted read-only at `/run/mnt/foo`


## chrootvm helper script

Try the `chrootvm` script which takes a chroot dir, finds the latest kernel in it
(from /usr/lib/modules), dynamically builds a suitable initrd and runs a
program in that kernel with the chroot as rootfs.

For example:

```bash
./chrootvm /vcs/other/osbuild/output/build /usr/bin/bash
```

If no progam is specified, the default is to run `/bin/sh`.

This also supports `--mount a-tag /a/path` and `--mount-ro a-tag /a/path` which creates
additional virtiofs mounts at `/run/mnt/tag`.

There is also a `--debug` option which will make the VM print debug output.
