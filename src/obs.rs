//! The parts of libobs the plugin uses, declared by hand: checked against
//! obs-studio's headers (master, 2026-11-24) and linked to `obs.dll` by
//! name (`raw-dylib`, so no import library is needed). Every function here
//! must be in obs.dll: Windows refuses to load a DLL with an import it
//! can't find, and OBS then skips the plugin.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_void};

macro_rules! opaque {
    ($($name:ident),*) => {
        $(
            #[repr(C)]
            pub struct $name {
                _private: [u8; 0],
            }
        )*
    };
}

opaque!(obs_module_t, obs_source_t, obs_data_t, obs_properties_t, obs_property_t, gs_effect_t, gs_eparam_t, gs_texture_t, proc_handler_t);

/// `MAKE_SEMANTIC_VERSION`: the libobs version the plugin is made for.
/// OBS 30.0 and later load it.
pub const fn semantic_version(major: u32, minor: u32, patch: u32) -> u32 {
    (major << 24) | (minor << 16) | patch
}

pub const OBS_SOURCE_TYPE_FILTER: c_int = 1;
pub const OBS_SOURCE_VIDEO: u32 = 1 << 0;

pub const OBS_COMBO_TYPE_LIST: c_int = 2;
pub const OBS_COMBO_TYPE_RADIO: c_int = 3;
pub const OBS_COMBO_FORMAT_INT: c_int = 1;
pub const OBS_TEXT_INFO: c_int = 3;
pub const OBS_TEXT_INFO_NORMAL: c_int = 0;
pub const OBS_TEXT_INFO_WARNING: c_int = 1;
pub const OBS_TEXT_INFO_ERROR: c_int = 2;

pub const OBS_EFFECT_OPAQUE: c_int = 2;

pub const LOG_ERROR: c_int = 100;
pub const LOG_WARNING: c_int = 200;
pub const LOG_INFO: c_int = 300;

type Unused = Option<unsafe extern "C" fn()>;

/// `struct obs_source_info`, up to `filter_remove`: registered with its
/// own size, OBS takes the fields after it as unset.
#[repr(C)]
pub struct obs_source_info {
    pub id: *const c_char,
    pub type_: c_int,
    pub output_flags: u32,
    pub get_name: Option<unsafe extern "C" fn(type_data: *mut c_void) -> *const c_char>,
    pub create: Option<unsafe extern "C" fn(settings: *mut obs_data_t, source: *mut obs_source_t) -> *mut c_void>,
    pub destroy: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub get_width: Option<unsafe extern "C" fn(data: *mut c_void) -> u32>,
    pub get_height: Option<unsafe extern "C" fn(data: *mut c_void) -> u32>,
    pub get_defaults: Option<unsafe extern "C" fn(settings: *mut obs_data_t)>,
    pub get_properties: Option<unsafe extern "C" fn(data: *mut c_void) -> *mut obs_properties_t>,
    pub update: Option<unsafe extern "C" fn(data: *mut c_void, settings: *mut obs_data_t)>,
    pub activate: Unused,
    pub deactivate: Unused,
    pub show: Unused,
    pub hide: Unused,
    pub video_tick: Option<unsafe extern "C" fn(data: *mut c_void, seconds: f32)>,
    pub video_render: Option<unsafe extern "C" fn(data: *mut c_void, effect: *mut gs_effect_t)>,
    pub filter_video: Unused,
    pub filter_audio: Unused,
    pub enum_active_sources: Unused,
    pub save: Unused,
    pub load: Unused,
    pub mouse_click: Unused,
    pub mouse_move: Unused,
    pub mouse_wheel: Unused,
    pub focus: Unused,
    pub key_click: Unused,
    pub filter_remove: Unused,
}

/// `struct obs_video_info` is written whole by `obs_get_video_info`, and it
/// may grow: read into room to spare, its first fields picked out.
#[repr(C, align(8))]
pub struct VideoInfo {
    bytes: [u8; 256],
}

impl VideoInfo {
    pub fn new() -> Self {
        Self { bytes: [0; 256] }
    }

    /// Frames a second, as a fraction: after `graphics_module` (a pointer).
    pub fn fps(&self) -> (u32, u32) {
        let at = std::mem::size_of::<*const c_char>();
        (self.u32_at(at), self.u32_at(at + 4))
    }

    /// The canvas's size (`base_width`, `base_height`): after the frame rate.
    pub fn base_size(&self) -> (u32, u32) {
        let at = std::mem::size_of::<*const c_char>() + 8;
        (self.u32_at(at), self.u32_at(at + 4))
    }

    fn u32_at(&self, i: usize) -> u32 {
        u32::from_ne_bytes(self.bytes[i..i + 4].try_into().unwrap())
    }
}

/// `obs_enum_canvases` and `obs_canvas_get_video_info`: OBS 31 and later
/// can have more canvases than the main one (Aitum Vertical's vertical
/// canvas is one). Looked up by name the first time they're wanted, as OBS
/// 30 doesn't have them, and Windows won't load a DLL with an import it
/// can't find.
pub struct Canvases {
    enumerate: EnumCanvases,
    video_info: CanvasVideoInfo,
}

/// `obs_enum_canvases`, `obs_canvas_get_video_info` (as in OBS's obs.h),
/// and what `GetProcAddress` hands back.
type EnumCanvases = unsafe extern "C" fn(enum_proc: unsafe extern "C" fn(param: *mut c_void, canvas: *mut c_void) -> bool, param: *mut c_void);
type CanvasVideoInfo = unsafe extern "C" fn(canvas: *const c_void, ovi: *mut VideoInfo) -> bool;
type Proc = unsafe extern "system" fn() -> isize;

impl Canvases {
    /// OBS's, if it has them.
    pub fn get() -> Option<&'static Canvases> {
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        use windows::core::{s, w};
        static FOUND: std::sync::OnceLock<Option<Canvases>> = std::sync::OnceLock::new();
        FOUND
            .get_or_init(|| unsafe {
                let obs = GetModuleHandleW(w!("obs.dll")).ok()?;
                let enumerate = GetProcAddress(obs, s!("obs_enum_canvases"))?;
                let video_info = GetProcAddress(obs, s!("obs_canvas_get_video_info"))?;
                Some(Canvases { enumerate: std::mem::transmute::<Proc, EnumCanvases>(enumerate), video_info: std::mem::transmute::<Proc, CanvasVideoInfo>(video_info) })
            })
            .as_ref()
    }

    /// The sizes of the canvases that have video (a vertical canvas has it
    /// while it streams or records), the main one among them.
    pub fn sizes(&self) -> Vec<(u32, u32)> {
        struct Walk {
            sizes: Vec<(u32, u32)>,
            video_info: CanvasVideoInfo,
        }
        unsafe extern "C" fn each(param: *mut c_void, canvas: *mut c_void) -> bool {
            let walk = unsafe { &mut *(param as *mut Walk) };
            let mut info = VideoInfo::new();
            if unsafe { (walk.video_info)(canvas, &mut info) } {
                walk.sizes.push(info.base_size());
            }
            true
        }
        let mut walk = Walk { sizes: Vec::new(), video_info: self.video_info };
        unsafe { (self.enumerate)(each, (&raw mut walk).cast()) };
        walk.sizes
    }
}

/// `struct calldata`: the arguments and results of a proc handler call.
#[repr(C)]
pub struct calldata_t {
    pub stack: *mut u8,
    pub size: usize,
    pub capacity: usize,
    pub fixed: bool,
}

impl calldata_t {
    pub fn new() -> Self {
        Self { stack: std::ptr::null_mut(), size: 0, capacity: 0, fixed: false }
    }
}

impl Drop for calldata_t {
    /// `calldata_free`.
    fn drop(&mut self) {
        if !self.fixed && !self.stack.is_null() {
            unsafe { bfree(self.stack.cast()) };
        }
    }
}

#[link(name = "obs", kind = "raw-dylib")]
unsafe extern "C" {
    pub fn blog(log_level: c_int, format: *const c_char, ...);
    pub fn bfree(ptr: *mut c_void);

    pub fn obs_register_source_s(info: *const obs_source_info, size: usize);
    pub fn obs_get_video_info(ovi: *mut VideoInfo) -> bool;
    pub fn obs_get_video_frame_time() -> u64;
    pub fn obs_enter_graphics();
    pub fn obs_leave_graphics();
    pub fn obs_get_base_effect(effect: c_int) -> *mut gs_effect_t;

    pub fn obs_filter_get_parent(filter: *const obs_source_t) -> *mut obs_source_t;
    pub fn obs_source_get_unversioned_id(source: *const obs_source_t) -> *const c_char;
    pub fn obs_source_get_settings(source: *const obs_source_t) -> *mut obs_data_t;
    pub fn obs_source_showing(source: *const obs_source_t) -> bool;
    pub fn obs_source_enabled(source: *const obs_source_t) -> bool;
    pub fn obs_source_get_base_width(source: *mut obs_source_t) -> u32;
    pub fn obs_source_get_base_height(source: *mut obs_source_t) -> u32;
    pub fn obs_source_skip_video_filter(filter: *mut obs_source_t);
    pub fn obs_source_get_proc_handler(source: *const obs_source_t) -> *mut proc_handler_t;
    pub fn obs_source_update_properties(source: *mut obs_source_t);

    pub fn proc_handler_call(handler: *mut proc_handler_t, name: *const c_char, params: *mut calldata_t) -> bool;
    pub fn calldata_get_data(data: *const calldata_t, name: *const c_char, out: *mut c_void, size: usize) -> bool;
    pub fn calldata_get_string(data: *const calldata_t, name: *const c_char, str: *mut *const c_char) -> bool;

    pub fn obs_data_get_int(data: *mut obs_data_t, name: *const c_char) -> i64;
    pub fn obs_data_get_bool(data: *mut obs_data_t, name: *const c_char) -> bool;
    pub fn obs_data_get_string(data: *mut obs_data_t, name: *const c_char) -> *const c_char;
    pub fn obs_data_set_int(data: *mut obs_data_t, name: *const c_char, val: i64);
    pub fn obs_data_set_default_int(data: *mut obs_data_t, name: *const c_char, val: i64);
    pub fn obs_data_release(data: *mut obs_data_t);

    pub fn obs_properties_create() -> *mut obs_properties_t;
    pub fn obs_properties_add_list(props: *mut obs_properties_t, name: *const c_char, description: *const c_char, type_: c_int, format: c_int) -> *mut obs_property_t;
    pub fn obs_property_list_add_int(p: *mut obs_property_t, name: *const c_char, val: i64) -> usize;
    pub fn obs_properties_add_text(props: *mut obs_properties_t, name: *const c_char, description: *const c_char, type_: c_int) -> *mut obs_property_t;
    pub fn obs_property_text_set_info_type(p: *mut obs_property_t, type_: c_int);
    pub fn obs_property_text_set_info_word_wrap(p: *mut obs_property_t, word_wrap: bool);
    pub fn obs_property_set_long_description(p: *mut obs_property_t, long_description: *const c_char);

    pub fn gs_get_device_obj() -> *mut c_void;
    pub fn gs_texture_open_nt_shared(handle: u32) -> *mut gs_texture_t;
    pub fn gs_texture_destroy(tex: *mut gs_texture_t);
    pub fn gs_texture_acquire_sync(tex: *mut gs_texture_t, key: u64, ms: u32) -> c_int;
    pub fn gs_texture_release_sync(tex: *mut gs_texture_t, key: u64) -> c_int;
    pub fn gs_effect_get_param_by_name(effect: *const gs_effect_t, name: *const c_char) -> *mut gs_eparam_t;
    pub fn gs_effect_set_texture(param: *mut gs_eparam_t, val: *mut gs_texture_t);
    pub fn gs_effect_loop(effect: *mut gs_effect_t, name: *const c_char) -> bool;
    pub fn gs_draw_sprite_subregion(tex: *mut gs_texture_t, flip: u32, x: u32, y: u32, cx: u32, cy: u32);
    pub fn gs_matrix_push();
    pub fn gs_matrix_pop();
    pub fn gs_matrix_scale3f(x: f32, y: f32, z: f32);
}

/// A line in OBS's log, starting `[hasteclip-obs]`.
pub fn log(level: c_int, message: &str) {
    let text = std::ffi::CString::new(format!("[hasteclip-obs] {message}").replace('\0', " ")).unwrap_or_default();
    unsafe { blog(level, c"%s".as_ptr(), text.as_ptr()) };
}

/// A C string from OBS as text (empty for null).
///
/// # Safety
/// `p` is null or a valid NUL-terminated string.
pub unsafe fn text(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// A source's settings, released when dropped.
pub struct Settings(pub *mut obs_data_t);

impl Settings {
    /// `source`'s settings (null if it has none).
    ///
    /// # Safety
    /// `source` is a live source.
    pub unsafe fn of(source: *const obs_source_t) -> Option<Self> {
        let data = unsafe { obs_source_get_settings(source) };
        (!data.is_null()).then_some(Self(data))
    }

    pub fn int(&self, name: &std::ffi::CStr) -> i64 {
        unsafe { obs_data_get_int(self.0, name.as_ptr()) }
    }

    pub fn bool(&self, name: &std::ffi::CStr) -> bool {
        unsafe { obs_data_get_bool(self.0, name.as_ptr()) }
    }

    pub fn string(&self, name: &std::ffi::CStr) -> String {
        unsafe { text(obs_data_get_string(self.0, name.as_ptr())) }
    }

    pub fn set_int(&self, name: &std::ffi::CStr, value: i64) {
        unsafe { obs_data_set_int(self.0, name.as_ptr(), value) }
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        unsafe { obs_data_release(self.0) };
    }
}
