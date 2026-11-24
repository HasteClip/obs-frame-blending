//! HasteClip Frame Blending for OBS Studio: a filter that gives a Game
//! Capture, Window Capture or Display Capture HasteClip's frame blending,
//! smooth motion from pictures taken several times faster than OBS's frame
//! rate. Windows for now: the capture and blending are HasteClip's own
//! (`capture::win::feed`), and the frames are handed to OBS as shared
//! Direct3D 11 textures.

#[cfg(target_os = "windows")]
mod filter;
#[cfg(target_os = "windows")]
mod find;
#[cfg(target_os = "windows")]
mod obs;

#[cfg(target_os = "windows")]
mod module {
    use std::ffi::c_char;
    use std::sync::atomic::{AtomicPtr, Ordering};

    use crate::obs::{self, obs_module_t};

    static MODULE: AtomicPtr<obs_module_t> = AtomicPtr::new(std::ptr::null_mut());

    // What OBS_DECLARE_MODULE() gives a plugin written in C.

    #[unsafe(no_mangle)]
    pub extern "C" fn obs_module_set_pointer(module: *mut obs_module_t) {
        MODULE.store(module, Ordering::Relaxed);
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn obs_current_module() -> *mut obs_module_t {
        MODULE.load(Ordering::Relaxed)
    }

    /// The libobs version it's made for: OBS 30.0 and later load it.
    #[unsafe(no_mangle)]
    pub extern "C" fn obs_module_ver() -> u32 {
        obs::semantic_version(30, 0, 0)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn obs_module_load() -> bool {
        crate::filter::register();
        obs::log(obs::LOG_INFO, concat!("version ", env!("CARGO_PKG_VERSION"), " loaded"));
        true
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn obs_module_unload() {}

    #[unsafe(no_mangle)]
    pub extern "C" fn obs_module_name() -> *const c_char {
        c"HasteClip Frame Blending".as_ptr()
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn obs_module_description() -> *const c_char {
        c"Frame blending for Game Capture, Window Capture and Display Capture, from HasteClip: smooth motion from pictures taken several times faster than OBS's frame rate.".as_ptr()
    }
}
