use nix::mount::{mount, umount, MsFlags};
use nix::sys::stat::{makedev, mknod, Mode, SFlag};
use nix::unistd::{chdir, chroot, execv};
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG: AtomicBool = AtomicBool::new(false);

fn set_debug(enabled: bool) {
    DEBUG.store(enabled, Ordering::Relaxed);
}

macro_rules! debugln {
    ($($arg:tt)*) => {
        if DEBUG.load(Ordering::Relaxed) {
            println!($($arg)*);
        }
    };
}

fn mkdir_p(path: &str) -> io::Result<()> {
    let path = Path::new(path);
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Create static device nodes
fn create_static_devices() -> Result<(), Box<dyn std::error::Error>> {
    debugln!("Creating static device nodes");

    // Table of device nodes to create
    // (path, major, minor, mode)
    let devices = [
        ("/dev/kvm", 10, 232, 0o660),
        ("/dev/loop-control", 10, 237, 0o660),
        ("/dev/fuse", 10, 229, 0o666),
    ];

    for (path, major, minor, mode) in &devices {
        debugln!("Creating {}", path);
        // Create parent directory if needed (for /dev/vfio/vfio)
        if let Some(parent) = Path::new(path).parent() {
            if parent != Path::new("/dev") {
                mkdir_p(parent.to_str().unwrap())?;
            }
        }

        let dev = makedev(*major, *minor);
        mknod(*path, SFlag::S_IFCHR, Mode::from_bits_truncate(*mode), dev)?;
    }

    let symlinks = [
        ("/dev/fd", "/proc/self/fd"),
        ("/dev/stdin", "/proc/self/fd/0"),
        ("/dev/stdout", "/proc/self/fd/1"),
        ("/dev/stderr", "/proc/self/fd/2"),
    ];

    for (link_path, target) in &symlinks {
        debugln!("Creating {}", link_path);
        symlink(target, link_path)?;
    }

    Ok(())
}

fn read_cmdline() -> io::Result<String> {
    debugln!("Creating /proc/cmdline");
    fs::read_to_string("/proc/cmdline").map(|s| s.trim().to_string())
}

fn cmdline_get<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    for param in cmdline.split_whitespace() {
        if param == key {
            return Some("");
        } else if let Some(value) = param.strip_prefix(&format!("{}=", key)) {
            return Some(value);
        }
    }
    None
}

/// Parse all mount= and mount-ro= parameters from cmdline
fn cmdline_get_mounts(cmdline: &str) -> Vec<(&str, bool)> {
    let mut mounts = Vec::new();

    for param in cmdline.split_whitespace() {
        if let Some(tag) = param.strip_prefix("mount=") {
            if !tag.is_empty() {
                mounts.push((tag, false));
            }
        } else if let Some(tag) = param.strip_prefix("mount-ro=") {
            if !tag.is_empty() {
                mounts.push((tag, true));
            }
        }
    }

    mounts
}

fn mount_apis() -> nix::Result<()> {
    let mounts = [
        (
            "sysfs",
            "/sys",
            "sysfs",
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
            None,
        ),
        (
            "devtmpfs",
            "/dev",
            "devtmpfs",
            MsFlags::MS_NOSUID,
            Some("seclabel,mode=0755,size=4m"),
        ),
        (
            "proc",
            "/proc",
            "proc",
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
            None,
        ),
        (
            "tmpfs",
            "/run",
            "tmpfs",
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            Some("seclabel,mode=0755,size=64m"),
        ),
        (
            "tmpfs",
            "/tmp",
            "tmpfs",
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            Some("seclabel,mode=0755,size=128m"),
        ),
    ];

    for (source, target, fstype, flags, data) in &mounts {
        debugln!("Mounting {}", *target);
        mount(Some(*source), *target, Some(*fstype), *flags, *data)?;
    }

    Ok(())
}

fn move_mount(src: &str, dest: &str) -> nix::Result<()> {
    debugln!("Moving {} to {}", src, dest);
    mount(
        Some(src),
        dest,
        None::<&str>,
        MsFlags::MS_MOVE,
        None::<&str>,
    )
}

fn mount_virtiofs(tag: &str, mountpoint: &str, read_only: bool) -> nix::Result<()> {
    debugln!(
        "Mounting {} at {} (read_only: {})",
        tag,
        mountpoint,
        read_only
    );
    let flags = if read_only {
        MsFlags::MS_RDONLY
    } else {
        MsFlags::empty()
    };

    mount(Some(tag), mountpoint, Some("virtiofs"), flags, None::<&str>)
}

fn switch_root(newroot: &str) -> Result<(), Box<dyn std::error::Error>> {
    debugln!("Switching root to {}", newroot);

    chdir(newroot)?;
    let _old_root = fs::File::open("/")?;
    move_mount(".", "/")?;
    chroot(".")?;
    chdir("/")?;
    Ok(())
}

fn load_kernel_module(module_path: &Path) -> io::Result<()> {
    debugln!("Loading module: {}", module_path.display());
    let module_data = fs::read(module_path)?;

    unsafe {
        let result = libc::syscall(
            libc::SYS_init_module,
            module_data.as_ptr(),
            module_data.len(),
            b"\0".as_ptr() as *const libc::c_char,
        );
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

/// Load all kernel modules from a directory, in named order
fn load_kernel_modules(modules_dir: &str) -> io::Result<()> {
    let dir_path = Path::new(modules_dir);
    if !dir_path.exists() {
        return Ok(());
    }

    let entries = fs::read_dir(dir_path)?;
    let mut module_paths = Vec::new();

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            if let Some(ext) = path.extension() {
                if ext == "ko" {
                    module_paths.push(path);
                }
            }
        }
    }

    // Sort modules by filename to ensure correct loading order
    module_paths.sort_by(|a, b| {
        let a_name = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let b_name = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
        a_name.cmp(b_name)
    });

    for path in module_paths {
        match load_kernel_module(&path) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("Failed to load module {}: {}", path.display(), e);
                // Continue loading other modules even if one fails
            }
        }
    }

    Ok(())
}

fn do_init() -> Result<(), Box<dyn std::error::Error>> {
    // Create required directories
    let dirs = ["/sysroot", "/sys", "/dev", "/proc", "/run", "/tmp"];
    for dir in &dirs {
        mkdir_p(dir)?;
    }

    mount_apis()?;

    mkdir_p("/run/mnt")?;

    let cmdline = read_cmdline()?;

    if cmdline_get(&cmdline, "debug").is_some() {
        set_debug(true);
    }

    create_static_devices()?;

    load_kernel_modules("/usr/lib/modules")?;

    let rootfs_tag = cmdline_get(&cmdline, "rootfs").unwrap_or("rootfs");
    mount_virtiofs(rootfs_tag, "/sysroot", true)?;

    let additional_mounts = cmdline_get_mounts(&cmdline);
    for (tag, read_only) in additional_mounts {
        let mount_path = format!("/run/mnt/{}", tag);
        mkdir_p(&mount_path)?;
        mount_virtiofs(tag, &mount_path, read_only)?;
    }

    // Move mounts to new root if mountpoint exists
    let surviving_mounts = ["/run", "/dev", "/proc", "/sys", "/tmp"];
    for mount_point in &surviving_mounts {
        let dest = format!("/sysroot{}", mount_point);
        if Path::new(&dest).exists() {
            move_mount(mount_point, &dest)?;
        } else {
            umount(mount_point)?;
        }
    }

    switch_root("/sysroot")?;

    let init_program = cmdline_get(&cmdline, "init").unwrap_or("/bin/sh");
    debugln!("Executing init: {}", init_program);

    // Get the basename for argv[0]
    let init_name = Path::new(init_program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(init_program);

    let init_path = CString::new(init_program)?;
    let init_arg = CString::new(init_name)?;
    let args = vec![&init_arg];

    execv(&init_path, &args)?;
    eprintln!("Failed to execute {}", init_program);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Err(err) = do_init() {
        eprintln!("Unexpected error: {}", err);
    }
    Ok(())
}
