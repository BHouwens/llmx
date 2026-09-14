//! Physical memory detection, used for advisory warnings only.
//!
//! Large local models sit very close to machine memory limits, so the picker
//! shows what a profile is expected to need next to what the machine has. It
//! is deliberately a *physical RAM* comparison: on Apple silicon the GPU
//! shares that pool, and trying to compute "available unified GPU memory"
//! accurately is not worth the complexity or the wrong answers.

/// Total physical memory in gigabytes (10^9 bytes), or `None` if unknown.
pub fn total_ram_gb() -> Option<f64> {
    total_ram_bytes().map(|bytes| bytes as f64 / 1_000_000_000.0)
}

#[cfg(target_os = "macos")]
fn total_ram_bytes() -> Option<u64> {
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let name = c"hw.memsize";

    // SAFETY: `hw.memsize` is a documented u64 sysctl. The out-pointer and the
    // size variable both refer to a live local of exactly that size, and the
    // name is a NUL-terminated C string.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut value as *mut u64 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };

    (rc == 0 && value > 0).then_some(value)
}

#[cfg(target_os = "linux")]
fn total_ram_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn total_ram_bytes() -> Option<u64> {
    None
}

/// How a profile's memory hints compare to this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryVerdict {
    /// No hints configured, or total RAM could not be read.
    Unknown,
    Comfortable,
    /// Fits, but with little headroom.
    Tight(String),
    /// Below the profile's stated minimum.
    Insufficient(String),
}

/// Compare a profile's hints against total physical RAM.
///
/// Advisory only — nothing in llmx blocks a launch on this. The estimates are
/// the user's own numbers, and "will it actually fit" depends on quantisation,
/// KV cache size and what else is running.
pub fn assess(
    hints: Option<&crate::config::ResourceHints>,
    total_gb: Option<f64>,
) -> MemoryVerdict {
    let (Some(hints), Some(total)) = (hints, total_gb) else {
        return MemoryVerdict::Unknown;
    };

    if let Some(min) = hints.min_ram_gb {
        if total < min {
            return MemoryVerdict::Insufficient(format!(
                "profile states a {min:.0} GB minimum; this machine has {total:.0} GB"
            ));
        }
    }

    if let Some(estimated) = hints.estimated_ram_gb {
        if estimated > total {
            return MemoryVerdict::Insufficient(format!(
                "estimated {estimated:.0} GB exceeds {total:.0} GB of physical RAM"
            ));
        }
        // Under ~8 GB of slack there is not much room for the OS, the editor
        // and the KV cache at long context.
        if total - estimated < 8.0 {
            return MemoryVerdict::Tight(format!(
                "estimated {estimated:.0} GB of {total:.0} GB leaves little headroom"
            ));
        }
    }

    if let Some(recommended) = hints.recommended_ram_gb {
        if total < recommended {
            return MemoryVerdict::Tight(format!(
                "below the recommended {recommended:.0} GB (this machine has {total:.0} GB)"
            ));
        }
    }

    MemoryVerdict::Comfortable
}
