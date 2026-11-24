"""macOS physical footprint (what Activity Monitor calls Memory), including Metal/unified allocations."""

import ctypes
import os

_libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")


def footprint_mb(pid: int | None = None) -> tuple[float, float]:
    """(phys_footprint, lifetime_max_phys_footprint) in MB, from rusage_info_v4."""
    buf = (ctypes.c_uint64 * 64)()
    _libc.proc_pid_rusage(ctypes.c_int(pid or os.getpid()), ctypes.c_int(4), buf)
    return buf[9] / 1048576, buf[30] / 1048576


def resident_mb(pid: int | None = None) -> float:
    """Resident size in MB; unlike footprint it includes mmap'd model files."""
    buf = (ctypes.c_uint64 * 64)()
    _libc.proc_pid_rusage(ctypes.c_int(pid or os.getpid()), ctypes.c_int(4), buf)
    return buf[8] / 1048576
