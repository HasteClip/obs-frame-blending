//! The Frame Blending filter: put on a Game Capture, Window Capture or
//! Display Capture, it captures the same game, window or display itself,
//! several times faster than OBS's frame rate, and draws each of OBS's
//! frames as the blend of the pictures in its time (HasteClip's frame
//! blending, `capture::win::feed`). OBS hands a filter one picture per
//! frame, so the pictures in between have to come from a capture of its own.
//!
//! Settings: the rate it captures at (a multiple of OBS's frame rate, kept
//! when that changes, see [`looks_for`]) and how a frame weighs its
//! pictures (Even or Soft, as in HasteClip). Its status line says what it's
//! doing, or why it isn't, and its last line who it's from ([`CREDIT`]).
//!
//! It blends at the size OBS's canvas shows its source, not bigger (a 1440p
//! game on a 1080p canvas at 1080p, see [`blend_size`]): most of the work
//! is copying and adding up pictures, and that's work for every pixel.
//!
//! OBS calls the filter from its graphics thread (tick, draw) and from its
//! interface thread (settings, size), which can each be waiting on the
//! other. So the filter's lock is only ever held for a moment, never while
//! calling anything that could wait: OBS's graphics (`obs_enter_graphics`),
//! OBS's other sources, or Windows about other programs' windows. And
//! drawing never waits on the blend: a frame still being written is passed
//! over for the one before.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;

use capture::BlendShape;
use capture::win::feed::{Feed, Frame, Options, Target};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::Interface;

use crate::find;
use crate::obs::{self, Settings, calldata_t, gs_effect_t, gs_texture_t, obs_data_t, obs_properties_t, obs_source_t, text};

pub const ID: &CStr = c"hasteclip_frame_blending";

/// The fastest it captures, as in HasteClip's Settings.
const MAX_BLEND: f64 = 2000.0;
/// The most pictures a frame, as in HasteClip's Settings.
const MAX_LOOKS: u32 = 16;
/// What it captures at to start with: four times OBS's 60 fps.
const DEFAULT_BLEND_FPS: i64 = 240;
/// How often it looks again at what its source captures.
const LOOK_EVERY: f32 = 0.5;
/// How long after a capture failed it tries again (seconds).
const RETRY_AFTER: f64 = 3.0;
/// How often the blending's numbers go to OBS's log, with
/// `HASTECLIP_BLEND_LOG=1` (see [`log_blend`]).
const LOG_EVERY: f64 = 5.0;

pub const BLEND_TIP: &CStr = c"How the pictures in each frame are weighed. Even: all the same, so a frame shows just its own moment, and a moving thing's trail ends sharply where the next frame's starts. Soft: the middle of the frame's moment counts most, fading out into the frames either side, so one frame hands over smoothly to the next, a little blurrier.";
const RATE_TIP: &CStr = c"Captures this many pictures a second and blends them into each of OBS's frames, for smooth motion like motion blur. Only whole multiples of OBS's frame rate, so every frame gets as many. When OBS's frame rate changes, it keeps capturing at about the same rate.";

/// Who it's from, the last line of its settings, "HasteClip" a link to the
/// plugin's page on HasteClip's site (OBS opens a link in an info text when
/// it's clicked). A credit, not an ad: HasteClip promises no ads.
const CREDIT: &CStr = c"From <a href=\"https://hasteclip.com/obs\">HasteClip</a>, the free clip recorder";

/// Register the filter with OBS.
pub fn register() {
    let info = obs::obs_source_info {
        id: ID.as_ptr(),
        type_: obs::OBS_SOURCE_TYPE_FILTER,
        output_flags: obs::OBS_SOURCE_VIDEO,
        get_name: Some(get_name),
        create: Some(create),
        destroy: Some(destroy),
        get_width: Some(get_width),
        get_height: Some(get_height),
        get_defaults: Some(get_defaults),
        get_properties: Some(get_properties),
        update: Some(update),
        activate: None,
        deactivate: None,
        show: None,
        hide: None,
        video_tick: Some(video_tick),
        video_render: Some(video_render),
        filter_video: None,
        filter_audio: None,
        enum_active_sources: None,
        save: None,
        load: None,
        mouse_click: None,
        mouse_move: None,
        mouse_wheel: None,
        focus: None,
        key_click: None,
        filter_remove: None,
    };
    unsafe { obs::obs_register_source_s(&info, std::mem::size_of::<obs::obs_source_info>()) };
}

/// Pictures a frame for a blend rate of `blend_fps` at OBS's `fps`: the
/// nearest whole multiple of OBS's frame rate, at least two, within
/// [`MAX_LOOKS`] and [`MAX_BLEND`]. One (no blending) when even two would be
/// too fast.
pub fn looks_for(blend_fps: f64, fps: f64) -> u32 {
    if fps <= 0.0 {
        return 1;
    }
    let most = ((MAX_BLEND / fps).floor() as u32).min(MAX_LOOKS);
    if most < 2 {
        return 1;
    }
    ((blend_fps / fps).round() as u32).clamp(2, most)
}

/// The blend rates offered at OBS's `fps`, with their pictures a frame.
pub fn rates(fps: f64) -> Vec<(i64, u32)> {
    (2..=MAX_LOOKS).map(|k| (rate(k, fps), k)).take_while(|(r, _)| *r as f64 <= MAX_BLEND).collect()
}

/// The blend rate of `looks` a frame at OBS's `fps`, as a setting: whole
/// pictures a second.
fn rate(looks: u32, fps: f64) -> i64 {
    (looks as f64 * fps).round() as i64
}

/// A rate as said in its menu: whole numbers as they are, fractional OBS
/// rates (59.94) to two places.
fn rate_text(looks: u32, fps: f64) -> String {
    let r = looks as f64 * fps;
    if (r - r.round()).abs() < 0.005 { format!("{}", r.round() as i64) } else { format!("{r:.2}") }
}

/// Whether a feed with room for `room` takes pictures `size` big (no
/// room given: room for any).
fn fits(room: Option<(u32, u32)>, size: Option<(u32, u32)>) -> bool {
    match (room, size) {
        (None, _) | (_, None) => true,
        (Some((rw, rh)), Some((w, h))) => w <= rw && h <= rh,
    }
}

/// The most a source's pictures are shrunk: to half their size. Shrunk
/// more in one go (a 4K game on a 720p canvas), a picture would lose detail
/// that OBS's own scaling keeps.
const LEAST_SCALE: f64 = 0.5;

/// How big a source `source` big is blended, and that as a scale of its
/// size: big enough to fill the biggest of OBS's `canvases`, as a canvas
/// smaller than the source shows it shrunk and blending it any bigger is
/// work thrown away; never bigger than the source, nor under half of it.
/// A source zoomed in past filling the canvas comes out a little softer.
fn blend_size(source: (u32, u32), canvases: &[(u32, u32)]) -> ((u32, u32), f64) {
    let (sw, sh) = (source.0 as f64, source.1 as f64);
    let fill = canvases.iter().filter(|&&(cw, ch)| cw > 0 && ch > 0).map(|&(cw, ch)| (cw as f64 / sw).max(ch as f64 / sh)).fold(0.0, f64::max);
    if fill == 0.0 || fill >= 1.0 {
        return (source, 1.0);
    }
    let scale = fill.max(LEAST_SCALE);
    // Rounded up: the capture shrinks a picture to fit by the smaller of
    // the two scales, which then isn't under this one.
    ((((sw * scale).ceil() as u32).min(source.0), ((sh * scale).ceil() as u32).min(source.1)), scale)
}

/// The size a frame `frame` big is drawn at: its source's size `source`
/// when it's the same shape (blended smaller, it's stretched back), else
/// its own size grown back by the `scale` its feed blends at (a window's
/// client area, when Window Capture shows the window's border too).
fn drawn_size(frame: (u32, u32), source: Option<(u32, u32)>, scale: f64) -> (u32, u32) {
    let (fw, fh) = (frame.0 as f64, frame.1 as f64);
    if let Some((w, h)) = source {
        // The same shape, give or take shrinking's rounding.
        if (fw * h as f64 - fh * w as f64).abs() <= 0.01 * fw * h as f64 {
            return (w, h);
        }
    }
    ((fw / scale).round() as u32, (fh / scale).round() as u32)
}

/// The sizes of OBS's canvases: the main one, and (OBS 31 and later) the
/// others that have video.
fn canvas_sizes() -> Vec<(u32, u32)> {
    let mut sizes = Vec::new();
    let mut info = obs::VideoInfo::new();
    if unsafe { obs::obs_get_video_info(&mut info) } {
        sizes.push(info.base_size());
    }
    if let Some(canvases) = obs::Canvases::get() {
        sizes.extend(canvases.sizes());
    }
    sizes
}

/// OBS's frame rate, as a fraction (none before OBS's video starts).
fn obs_fps() -> Option<(u32, u32)> {
    let mut info = obs::VideoInfo::new();
    if !unsafe { obs::obs_get_video_info(&mut info) } {
        return None;
    }
    let (num, den) = info.fps();
    (num > 0 && den > 0).then_some((num, den))
}

/// The LUID of the graphics card OBS draws on, if it draws with Direct3D 11.
fn obs_adapter() -> Option<u64> {
    unsafe { obs::obs_enter_graphics() };
    let device = unsafe { obs::gs_get_device_obj() };
    unsafe { obs::obs_leave_graphics() };
    if device.is_null() {
        return None;
    }
    let device = unsafe { ID3D11Device::from_raw_borrowed(&device) }?;
    let dxgi: IDXGIDevice = device.cast().ok()?;
    let desc = unsafe { dxgi.GetAdapter().and_then(|a| a.GetDesc()) }.ok()?;
    Some(((desc.AdapterLuid.HighPart as u32 as u64) << 32) | desc.AdapterLuid.LowPart as u64)
}

/// The time on OBS's clock (`os_gettime_ns`), in host seconds.
fn host_now() -> f64 {
    unsafe { obs::obs_get_video_frame_time() as f64 / 1e9 }
}

/// What the filter's source captures, for blending; or why it can't be.
enum Wants {
    Blend { target: Target, cursor: bool },
    /// Can't now (it says why), but may later: tried again.
    Waiting(String),
    /// Never on this source.
    NotHere(String),
}

/// A window found for a name, kept so it's looked for again only when the
/// name changes or the window's gone.
type Known = Option<(find::WindowName, find::Found)>;

/// The window named `name`: the one known, while it's still there, else
/// looked for (and then told in `found`).
fn window_for(name: &find::WindowName, known: &Known, found: &mut Known) -> Option<isize> {
    if let Some((n, w)) = known
        && n == name
        && w.still_there()
    {
        return Some(w.window);
    }
    let w = find::window(name)?;
    *found = Some((name.clone(), w));
    Some(w.window)
}

/// What `parent` (the filter's source) captures.
///
/// # Safety
/// `parent` is a live source.
unsafe fn wants(parent: *mut obs_source_t, known: &Known, found: &mut Known) -> Wants {
    let id = unsafe { text(obs::obs_source_get_unversioned_id(parent)) };
    let Some(settings) = (unsafe { Settings::of(parent) }) else {
        return Wants::Waiting("its source has no settings yet".into());
    };
    match id.as_str() {
        "game_capture" => {
            let cursor = settings.bool(c"capture_cursor");
            match unsafe { hooked(parent) } {
                Some(name) => match window_for(&name, known, found) {
                    Some(window) => Wants::Blend { target: Target::HookedGame { window }, cursor },
                    None => Wants::Waiting(format!("can't find {}'s window", name.exe)),
                },
                None => Wants::Waiting("waiting for Game Capture to capture a game".into()),
            }
        }
        "window_capture" => match find::WindowName::from_setting(&settings.string(c"window")) {
            Some(name) => match window_for(&name, known, found) {
                Some(window) => Wants::Blend { target: Target::Window { window }, cursor: true },
                None => Wants::Waiting(format!("waiting for {}'s window", name.exe)),
            },
            None => Wants::Waiting("Window Capture has no window picked".into()),
        },
        "monitor_capture" => Wants::Blend { target: Target::Display { monitor: find::display(&settings.string(c"monitor_id")) }, cursor: true },
        _ => Wants::NotHere("Frame blending works on Game Capture, Window Capture and Display Capture.".into()),
    }
}

/// The window a Game Capture has hooked, as it names it (none: not
/// capturing a game).
///
/// # Safety
/// `parent` is a live Game Capture.
unsafe fn hooked(parent: *mut obs_source_t) -> Option<find::WindowName> {
    let handler = unsafe { obs::obs_source_get_proc_handler(parent) };
    if handler.is_null() {
        return None;
    }
    let mut call = calldata_t::new();
    if !unsafe { obs::proc_handler_call(handler, c"get_hooked".as_ptr(), &mut call) } {
        return None;
    }
    let mut is_hooked = false;
    let got = unsafe { obs::calldata_get_data(&call, c"hooked".as_ptr(), (&mut is_hooked as *mut bool).cast(), 1) };
    if !got || !is_hooked {
        return None;
    }
    let string = |name: &CStr| {
        let mut s: *const c_char = std::ptr::null();
        unsafe { obs::calldata_get_string(&call, name.as_ptr(), &mut s) };
        unsafe { text(s) }
    };
    Some(find::WindowName { title: string(c"title"), class: string(c"class"), exe: string(c"executable") })
}

/// A running feed and what it was started for.
struct Running {
    feed: Feed,
    target: Target,
    period_ns: u64,
    cursor: bool,
    looks: u32,
    soft: bool,
    /// The biggest picture it has room for (a bigger one is shrunk to fit),
    /// and that as a scale of its source's size then.
    size: Option<(u32, u32)>,
    scale: f64,
}

/// The filter's status line, and how it's shown.
#[derive(Clone, PartialEq, Default)]
struct Line {
    text: String,
    kind: i32,
}

struct State {
    /// The blend rate set (whole pictures a second), and Soft (else Even).
    blend_fps: i64,
    soft: bool,
    /// OBS's graphics card (none: OBS doesn't draw with Direct3D 11).
    adapter: Option<u64>,
    running: Option<Running>,
    /// The running feed's textures, opened in OBS's graphics by handle.
    textures: HashMap<isize, *mut gs_texture_t>,
    /// Seconds till it looks again at what its source captures.
    look_in: f32,
    /// When to try again after a feed failed (OBS's clock, seconds).
    retry_at: f64,
    /// Why the last feed stopped, said till the next one runs.
    failed: Option<String>,
    /// The window last found for its source.
    known: Known,
    line: Line,
    /// The last frame's size, as drawn.
    shown: Option<(u32, u32)>,
    /// When the blending's numbers were last logged, and the frames made by
    /// then (see [`log_blend`]).
    logged: Option<(f64, u64)>,
}

struct Filter {
    me: *mut obs_source_t,
    state: Mutex<State>,
}

unsafe impl Send for Filter {}
unsafe impl Sync for Filter {}

impl Filter {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// What to let go of once the lock is let go of: textures to destroy (in
/// OBS's graphics, which may be busy) and a feed to stop (which waits for
/// its capture to stop).
#[derive(Default)]
struct Letting {
    textures: Vec<*mut gs_texture_t>,
    feeds: Vec<Running>,
}

/// Take the running feed and its textures out of `s`, to let go of.
fn retire(s: &mut State, into: &mut Letting) {
    s.shown = None;
    s.logged = None;
    into.textures.extend(s.textures.drain().map(|(_, tex)| tex));
    into.feeds.extend(s.running.take());
}

/// Let go of them: never with the lock held.
fn let_go(letting: Letting) {
    if !letting.textures.is_empty() {
        unsafe { obs::obs_enter_graphics() };
        for tex in letting.textures {
            unsafe { obs::gs_texture_destroy(tex) };
        }
        unsafe { obs::obs_leave_graphics() };
    }
    for r in letting.feeds {
        let _ = std::thread::Builder::new().name("blend feed stop".into()).spawn(move || drop(r));
    }
}

/// Run an OBS callback, a panic logged rather than taking OBS down with it.
fn guard<T>(what: &str, fallback: T, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(e) => {
            let why = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_default();
            obs::log(obs::LOG_ERROR, &format!("{what} failed: {why}"));
            fallback
        }
    }
}

/// # Safety
/// `data` is what [`create`] returned.
unsafe fn filter<'a>(data: *mut c_void) -> &'a Filter {
    unsafe { &*(data as *const Filter) }
}

unsafe extern "C" fn get_name(_type_data: *mut c_void) -> *const c_char {
    c"Frame Blending".as_ptr()
}

unsafe extern "C" fn create(settings: *mut obs_data_t, source: *mut obs_source_t) -> *mut c_void {
    guard("creating the filter", std::ptr::null_mut(), || {
        let adapter = obs_adapter();
        let state = State {
            blend_fps: DEFAULT_BLEND_FPS,
            soft: false,
            adapter,
            running: None,
            textures: HashMap::new(),
            look_in: 0.0,
            retry_at: 0.0,
            failed: None,
            known: None,
            line: Line::default(),
            shown: None,
            logged: None,
        };
        let f = Box::new(Filter { me: source, state: Mutex::new(state) });
        let data = Box::into_raw(f) as *mut c_void;
        unsafe { update(data, settings) };
        data
    })
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    guard("removing the filter", (), || {
        let f = unsafe { Box::from_raw(data as *mut Filter) };
        let mut letting = Letting::default();
        retire(&mut f.lock(), &mut letting);
        let_go(letting);
    })
}

unsafe extern "C" fn get_defaults(settings: *mut obs_data_t) {
    unsafe {
        obs::obs_data_set_default_int(settings, c"blend_fps".as_ptr(), DEFAULT_BLEND_FPS);
        obs::obs_data_set_default_int(settings, c"blend_shape".as_ptr(), 0);
    }
}

unsafe extern "C" fn update(data: *mut c_void, settings: *mut obs_data_t) {
    guard("changing settings", (), || {
        let f = unsafe { filter(data) };
        let blend_fps = unsafe { obs::obs_data_get_int(settings, c"blend_fps".as_ptr()) };
        let soft = unsafe { obs::obs_data_get_int(settings, c"blend_shape".as_ptr()) } == 1;
        let mut s = f.lock();
        (s.blend_fps, s.soft) = (blend_fps.max(1), soft);
        // Taken up at once.
        s.look_in = 0.0;
    })
}

unsafe extern "C" fn get_properties(data: *mut c_void) -> *mut obs_properties_t {
    guard("showing settings", std::ptr::null_mut(), || {
        let f = unsafe { filter(data) };
        let line = f.lock().line.clone();
        let fps = obs_fps().map(|(n, d)| n as f64 / d as f64).unwrap_or(60.0);
        unsafe {
            let props = obs::obs_properties_create();
            let list = obs::obs_properties_add_list(props, c"blend_fps".as_ptr(), c"Frame blending".as_ptr(), obs::OBS_COMBO_TYPE_LIST, obs::OBS_COMBO_FORMAT_INT);
            obs::obs_property_set_long_description(list, RATE_TIP.as_ptr());
            for (value, looks) in rates(fps) {
                let label = CString::new(format!("From {} fps (×{looks})", rate_text(looks, fps))).unwrap_or_default();
                obs::obs_property_list_add_int(list, label.as_ptr(), value);
            }
            let shape = obs::obs_properties_add_list(props, c"blend_shape".as_ptr(), c"Blend".as_ptr(), obs::OBS_COMBO_TYPE_RADIO, obs::OBS_COMBO_FORMAT_INT);
            obs::obs_property_set_long_description(shape, BLEND_TIP.as_ptr());
            obs::obs_property_list_add_int(shape, c"Even".as_ptr(), 0);
            obs::obs_property_list_add_int(shape, c"Soft".as_ptr(), 1);
            if !line.text.is_empty() {
                let text = CString::new(line.text).unwrap_or_default();
                let info = obs::obs_properties_add_text(props, c"status".as_ptr(), text.as_ptr(), obs::OBS_TEXT_INFO);
                obs::obs_property_text_set_info_type(info, line.kind);
                obs::obs_property_text_set_info_word_wrap(info, true);
            }
            let info = obs::obs_properties_add_text(props, c"credit".as_ptr(), CREDIT.as_ptr(), obs::OBS_TEXT_INFO);
            obs::obs_property_text_set_info_word_wrap(info, true);
            props
        }
    })
}

/// What the filter had when it looked, taken under the lock.
struct Seen {
    blend_fps: i64,
    adapter: Option<u64>,
    known: Known,
}

/// What it found, looked at without the lock.
struct Plan {
    fps: f64,
    period_ns: u64,
    looks: u32,
    /// The rate as set, kept to one the menu offers at OBS's frame rate.
    snapped: Option<i64>,
    wants: Wants,
    /// How big the source is blended now, and that as a scale of its size
    /// ([`blend_size`]).
    size: Option<(u32, u32)>,
    scale: f64,
    /// A window newly found for the source.
    found: Known,
    now: f64,
}

unsafe extern "C" fn video_tick(data: *mut c_void, seconds: f32) {
    guard("following its source", (), || {
        let f = unsafe { filter(data) };
        let seen = {
            let mut s = f.lock();
            s.look_in -= seconds;
            if s.look_in > 0.0 {
                return;
            }
            s.look_in = LOOK_EVERY;
            Seen { blend_fps: s.blend_fps, adapter: s.adapter, known: s.known.clone() }
        };
        let Some(plan) = look(f, &seen) else { return };
        let snapped = plan.snapped;
        let mut letting = Letting::default();
        let changed = {
            let mut s = f.lock();
            let before = s.line.clone();
            follow(&mut s, plan, &mut letting);
            s.line != before
        };
        let_go(letting);
        if let Some(rate) = snapped
            && let Some(settings) = unsafe { Settings::of(f.me) }
        {
            settings.set_int(c"blend_fps", rate);
        }
        // An open settings window asks for them again.
        if changed {
            unsafe { obs::obs_source_update_properties(f.me) };
        }
    })
}

/// Look at OBS's frame rate and what the source captures: without the
/// filter's lock, as it calls into OBS's sources and looks at windows.
fn look(f: &Filter, seen: &Seen) -> Option<Plan> {
    let (num, den) = obs_fps()?;
    let fps = num as f64 / den as f64;
    // OBS's frame interval, as OBS works it out.
    let period_ns = 1_000_000_000u64 * den as u64 / num as u64;
    let looks = looks_for(seen.blend_fps as f64, fps);
    let snapped = (looks > 1 && rate(looks, fps) != seen.blend_fps).then(|| rate(looks, fps));
    let parent = unsafe { obs::obs_filter_get_parent(f.me) };
    let mut found = None;
    let wants = if parent.is_null() {
        Wants::Waiting("not on a source yet".into())
    } else if !unsafe { obs::obs_source_enabled(f.me) } {
        // Turned off with its eye: OBS ticks it all the same, and a feed
        // left running cost as much as one blending (measured).
        Wants::Waiting("it's turned off".into())
    } else if seen.adapter.is_none() {
        Wants::NotHere("Frame blending needs OBS to draw with Direct3D 11 (Settings → Advanced → Video → Renderer).".into())
    } else if looks < 2 {
        Wants::NotHere(format!("OBS's frame rate ({}) is too high to blend: that would mean capturing faster than {MAX_BLEND} fps.", rate_text(1, fps)))
    } else if !unsafe { obs::obs_source_showing(parent) } {
        Wants::Waiting("its source isn't shown".into())
    } else {
        unsafe { wants(parent, &seen.known, &mut found) }
    };
    let blend = source_size(parent).map(|source| blend_size(source, &canvas_sizes()));
    Some(Plan { fps, period_ns, looks, snapped, wants, size: blend.map(|b| b.0), scale: blend.map_or(1.0, |b| b.1), found, now: host_now() })
}

/// The filter's source's size (none before it has a picture).
fn source_size(parent: *mut obs_source_t) -> Option<(u32, u32)> {
    if parent.is_null() {
        return None;
    }
    let (w, h) = unsafe { (obs::obs_source_get_base_width(parent), obs::obs_source_get_base_height(parent)) };
    (w >= 2 && h >= 2).then_some((w, h))
}

/// Start, change or stop the feed to match what was found; then say how
/// it's doing. Under the lock: quick, and calls nothing that waits (what's
/// let go of goes into `letting`).
fn follow(s: &mut State, plan: Plan, letting: &mut Letting) {
    let Plan { fps, period_ns, looks, snapped, wants, size, scale, found, now } = plan;
    if let Some(rate) = snapped {
        s.blend_fps = rate;
    }
    if found.is_some() {
        s.known = found;
    }
    // A feed that stopped by itself: why, and when to try again.
    if let Some(why) = s.running.as_ref().and_then(|r| r.feed.status().ended) {
        obs::log(obs::LOG_WARNING, &format!("blending stopped: {why}"));
        s.failed = Some(why);
        s.retry_at = now + RETRY_AFTER;
        retire(s, letting);
    }
    match wants {
        Wants::Blend { target, cursor } => {
            let same = s.running.as_ref().is_some_and(|r| r.target == target && r.period_ns == period_ns && r.cursor == cursor && fits(r.size, size));
            if same {
                let soft = s.soft;
                let r = s.running.as_mut().unwrap();
                if r.looks != looks || r.soft != soft {
                    r.feed.set_blend(looks, if soft { BlendShape::Soft } else { BlendShape::Even });
                    (r.looks, r.soft) = (looks, soft);
                }
            } else if size.is_none() {
                // Started once its source has a picture, as how big that is
                // says how big to blend ([`blend_size`]): Window Capture
                // has none for a moment after it finds its window.
                retire(s, letting);
                s.line = Line { text: "Not blending: waiting for its source's first picture.".into(), kind: obs::OBS_TEXT_INFO_NORMAL };
                return;
            } else if now >= s.retry_at {
                retire(s, letting);
                let shape = if s.soft { BlendShape::Soft } else { BlendShape::Even };
                let options = Options { t0: now, period_ns, looks, shape, cursor, adapter: s.adapter, size };
                obs::log(obs::LOG_INFO, &format!("blending {target:?} at {size:?} ({:.0}% of its size): {looks} pictures a frame at {} fps", scale * 100.0, rate_text(1, fps)));
                s.running = Some(Running { feed: Feed::start(target, options), target, period_ns, cursor, looks, soft: s.soft, size, scale });
            }
        }
        Wants::Waiting(why) => {
            retire(s, letting);
            s.line = Line { text: format!("Not blending: {why}."), kind: obs::OBS_TEXT_INFO_NORMAL };
            return;
        }
        Wants::NotHere(why) => {
            retire(s, letting);
            s.line = Line { text: why, kind: obs::OBS_TEXT_INFO_WARNING };
            return;
        }
    }
    s.line = match (&s.running, &s.failed) {
        (Some(r), _) => {
            if r.feed.status().frames == 0 {
                Line { text: format!("Starting: capturing at {} fps.", rate_text(looks, fps)), kind: obs::OBS_TEXT_INFO_NORMAL }
            } else {
                s.failed = None;
                Line { text: format!("Captured at {} fps, blended down to OBS's {} fps.", rate_text(looks, fps), rate_text(1, fps)), kind: obs::OBS_TEXT_INFO_NORMAL }
            }
        }
        (None, Some(why)) => Line { text: format!("Not blending: {why}. Trying again."), kind: obs::OBS_TEXT_INFO_ERROR },
        (None, None) => Line::default(),
    };
    log_blend(s, now);
}

/// The blending's numbers in OBS's log every [`LOG_EVERY`] seconds, with
/// `HASTECLIP_BLEND_LOG=1`, as HasteClip logs its own: the frames made, the
/// source's new pictures a second, and how many different pictures each
/// frame's looks saw (all of them, when the source keeps up).
fn log_blend(s: &mut State, now: f64) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("HASTECLIP_BLEND_LOG").is_some_and(|v| v != "0")) {
        return;
    }
    let Some(r) = &s.running else { return };
    let st = r.feed.status();
    match s.logged {
        Some((at, _)) if now - at < LOG_EVERY => {}
        Some((at, frames)) => {
            let made = st.frames.saturating_sub(frames);
            obs::log(
                obs::LOG_INFO,
                &format!(
                    "blending: {made} frames in {:.1} s, {} new pictures a second, {:.2} different a frame of {} (fewest {}), shown {} frames late",
                    now - at,
                    st.pictures,
                    st.different,
                    st.looks,
                    st.fewest,
                    st.delay
                ),
            );
            s.logged = Some((now, st.frames));
        }
        None => s.logged = Some((now, st.frames)),
    }
}

unsafe extern "C" fn video_render(data: *mut c_void, _effect: *mut gs_effect_t) {
    guard("drawing", (), || {
        let f = unsafe { filter(data) };
        // The frame to show and the one before it (in case its texture is
        // still being written), with their textures: picked under the lock
        // (OBS's graphics are held here already); drawn outside it, as OBS
        // may ask the filter's size meanwhile.
        let (candidates, scale): (Vec<(Frame, *mut gs_texture_t)>, f64) = {
            let mut s = f.lock();
            let t = host_now();
            let (frames, scale): (Vec<Frame>, f64) = match &s.running {
                Some(r) => {
                    let first = r.feed.frame_for(t);
                    let before = first.and_then(|fr| r.feed.frame_before(fr.index));
                    ([first, before].into_iter().flatten().collect(), r.scale)
                }
                None => (Vec::new(), 1.0),
            };
            let candidates = frames
                .into_iter()
                .filter_map(|frame| {
                    let tex = match s.textures.get(&frame.handle) {
                        Some(tex) => *tex,
                        None => {
                            let tex = unsafe { obs::gs_texture_open_nt_shared(frame.handle as u32) };
                            if tex.is_null() {
                                return None;
                            }
                            s.textures.insert(frame.handle, tex);
                            tex
                        }
                    };
                    Some((frame, tex))
                })
                .collect();
            (candidates, scale)
        };
        let source = source_size(unsafe { obs::obs_filter_get_parent(f.me) });
        let drawn = candidates.into_iter().find_map(|(frame, tex)| {
            let size = drawn_size((frame.width, frame.height), source, scale);
            unsafe { draw(tex, frame.width, frame.height, size) }.then_some(size)
        });
        match drawn {
            Some(size) => f.lock().shown = Some(size),
            None => unsafe { obs::obs_source_skip_video_filter(f.me) },
        }
    })
}

/// Draw `width`×`height` of a shared frame from its top-left corner,
/// stretched to `size` (its source's size, when it was blended smaller),
/// opaque (a game's pictures can have anything in their alpha). False, at
/// once, if its texture is being written.
///
/// # Safety
/// In OBS's graphics; `tex` an open shared texture.
unsafe fn draw(tex: *mut gs_texture_t, width: u32, height: u32, size: (u32, u32)) -> bool {
    unsafe {
        if obs::gs_texture_acquire_sync(tex, 0, 0) != 0 {
            return false;
        }
        let stretched = size != (width, height);
        if stretched {
            obs::gs_matrix_push();
            obs::gs_matrix_scale3f(size.0 as f32 / width as f32, size.1 as f32 / height as f32, 1.0);
        }
        let effect = obs::obs_get_base_effect(obs::OBS_EFFECT_OPAQUE);
        let image = obs::gs_effect_get_param_by_name(effect, c"image".as_ptr());
        obs::gs_effect_set_texture(image, tex);
        while obs::gs_effect_loop(effect, c"Draw".as_ptr()) {
            obs::gs_draw_sprite_subregion(tex, 0, 0, 0, width, height);
        }
        if stretched {
            obs::gs_matrix_pop();
        }
        obs::gs_texture_release_sync(tex, 0);
    }
    true
}

/// The filter's size: the blended frame's as drawn, else its source's.
fn size(data: *mut c_void) -> (u32, u32) {
    let f = unsafe { filter(data) };
    let shown = f.lock().shown;
    if let Some(size) = shown {
        return size;
    }
    let parent = unsafe { obs::obs_filter_get_parent(f.me) };
    if parent.is_null() {
        return (0, 0);
    }
    unsafe { (obs::obs_source_get_base_width(parent), obs::obs_source_get_base_height(parent)) }
}

unsafe extern "C" fn get_width(data: *mut c_void) -> u32 {
    guard("sizing", 0, || size(data).0)
}

unsafe extern "C" fn get_height(data: *mut c_void) -> u32 {
    guard("sizing", 0, || size(data).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rate kept when OBS's frame rate changes: the nearest whole
    /// multiple, at least two, within the limits.
    #[test]
    fn rate_follows_obs() {
        assert_eq!(looks_for(240.0, 60.0), 4);
        assert_eq!(looks_for(240.0, 30.0), 8);
        assert_eq!(looks_for(240.0, 144.0), 2);
        assert_eq!(looks_for(300.0, 144.0), 2);
        assert_eq!(looks_for(240.0, 59.94), 4);
        assert_eq!(looks_for(10_000.0, 60.0), 16);
        assert_eq!(looks_for(10_000.0, 240.0), 8);
        assert_eq!(looks_for(60.0, 60.0), 2);
        assert_eq!(looks_for(240.0, 1200.0), 1);
    }

    #[test]
    fn rates_offered() {
        assert_eq!(rates(60.0).first(), Some(&(120, 2)));
        assert_eq!(rates(60.0).last(), Some(&(960, 16)));
        assert_eq!(rates(240.0).last(), Some(&(1920, 8)));
        assert_eq!(rates(59.94)[2], (240, 4));
        assert!(rates(1200.0).is_empty());
        assert_eq!(rate_text(4, 59.94), "239.76");
        assert_eq!(rate_text(4, 60.0), "240");
    }

    /// Blended as big as it takes to fill the biggest canvas with video: a
    /// 1440p game on a 1080p canvas at 1080p, an ultrawide one filling the
    /// canvas's height; never bigger than the source, nor under half of it;
    /// whole while a vertical canvas streams (it shows the game zoomed in).
    #[test]
    fn blended_at_the_canvas_size() {
        let hd = (1920, 1080);
        assert_eq!(blend_size((2560, 1440), &[hd]), ((1920, 1080), 0.75));
        assert_eq!(blend_size((2560, 1440), &[hd, hd]), ((1920, 1080), 0.75));
        assert_eq!(blend_size((1920, 1080), &[hd]), ((1920, 1080), 1.0));
        assert_eq!(blend_size((1280, 720), &[hd]), ((1280, 720), 1.0));
        assert_eq!(blend_size((3440, 1440), &[hd]), ((2580, 1080), 0.75));
        assert_eq!(blend_size((2560, 1600), &[hd]), ((1920, 1200), 0.75));
        assert_eq!(blend_size((2560, 1440), &[hd, (1080, 1920)]), ((2560, 1440), 1.0));
        assert_eq!(blend_size((3840, 2160), &[(1280, 720)]), ((1920, 1080), 0.5));
        assert_eq!(blend_size((2560, 1440), &[]), ((2560, 1440), 1.0));
        assert_eq!(blend_size((2560, 1440), &[(0, 0)]), ((2560, 1440), 1.0));
    }

    /// Drawn at its source's size when blended smaller, so the scene keeps
    /// its layout; a window's client area (Window Capture showing the
    /// window's border too) at its own size, grown back by its feed's scale.
    #[test]
    fn drawn_at_the_source_size() {
        assert_eq!(drawn_size((1920, 1080), Some((2560, 1440)), 0.75), (2560, 1440));
        // The game went to 4K since: shrunk more, still drawn whole.
        assert_eq!(drawn_size((1920, 1080), Some((3840, 2160)), 0.75), (3840, 2160));
        assert_eq!(drawn_size((954, 536), Some((1366, 768)), 0.7), (1366, 768));
        assert_eq!(drawn_size((1920, 1080), Some((1936, 1119)), 1.0), (1920, 1080));
        assert_eq!(drawn_size((1920, 1080), Some((2576, 1479)), 0.745), (2577, 1450));
        assert_eq!(drawn_size((1920, 1080), None, 0.75), (2560, 1440));
    }

    /// A feed is started again only when its pictures outgrow its room.
    #[test]
    fn room() {
        assert!(fits(Some((1920, 1080)), Some((1280, 720))));
        assert!(!fits(Some((1280, 720)), Some((1920, 1080))));
        assert!(!fits(Some((1920, 1080)), Some((1920, 1200))));
        assert!(fits(None, Some((7680, 4320))));
        assert!(fits(Some((800, 600)), None));
    }
}
