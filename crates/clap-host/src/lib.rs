//! A small CLAP host: enough to run decks, nothing a deck doesn't need.
//!
//! - [`Bundle::load`] opens a `.clap` and initialises its entry.
//! - [`Bundle::instantiate`] makes an [`Instance`] (created and `init`ed on
//!   the calling thread, which this host treats as the main thread).
//! - On the main thread: parameters ([`Instance::params`]), state
//!   ([`Instance::save_state`] / [`Instance::load_state`]), `activate`, and
//!   [`Instance::idle`] to run the plugin's main-thread callbacks.
//! - [`Instance::processor`] hands out the audio-thread half, which starts
//!   processing on its first [`Processor::process`] and stops when dropped.
//!
//! Threads follow CLAP's rules: the host answers `clap.thread-check` with
//! the thread that made the instance as main, any other as audio.

use anyhow::{anyhow, bail, Context, Result};
use clap_sys::{
    audio_buffer::clap_audio_buffer,
    entry::clap_plugin_entry,
    events::*,
    ext::{params::*, state::*, thread_check::*},
    factory::plugin_factory::*,
    host::clap_host,
    plugin::clap_plugin,
    process::*,
    stream::{clap_istream, clap_ostream},
    version::CLAP_VERSION,
};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::ThreadId;

/// An opened `.clap` file.
pub struct Bundle {
    path: PathBuf,
    entry: *const clap_plugin_entry,
    factory: *const clap_plugin_factory,
    _lib: libloading::Library,
}

unsafe impl Send for Bundle {}
unsafe impl Sync for Bundle {}

/// A plugin the bundle offers.
#[derive(Clone, Debug)]
pub struct Descriptor {
    pub id: String,
    pub name: String,
    pub version: String,
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

impl Bundle {
    pub fn load(path: &Path) -> Result<Bundle> {
        let lib = unsafe { libloading::Library::new(path) }.with_context(|| format!("dlopen {}", path.display()))?;
        let entry: *const clap_plugin_entry = unsafe {
            *lib.get::<*const clap_plugin_entry>(b"clap_entry\0").context("no clap_entry symbol")?
        };
        // `clap_entry` is the struct itself: the symbol's address.
        let entry = if entry.is_null() { bail!("null clap_entry") } else { entry };
        let e = unsafe { &*entry };
        let p = CString::new(path.to_string_lossy().as_bytes())?;
        if !unsafe { (e.init.ok_or_else(|| anyhow!("entry has no init"))?)(p.as_ptr()) } {
            bail!("{}: entry init failed", path.display());
        }
        let factory = unsafe { (e.get_factory.ok_or_else(|| anyhow!("no get_factory"))?)(CLAP_PLUGIN_FACTORY_ID.as_ptr()) }
            as *const clap_plugin_factory;
        if factory.is_null() { bail!("{}: no plugin factory", path.display()); }
        Ok(Bundle { path: path.to_owned(), entry, factory, _lib: lib })
    }

    pub fn path(&self) -> &Path { &self.path }

    pub fn descriptors(&self) -> Vec<Descriptor> {
        let f = unsafe { &*self.factory };
        let n = unsafe { f.get_plugin_count.map_or(0, |c| c(self.factory)) };
        (0..n).filter_map(|i| {
            let d = unsafe { f.get_plugin_descriptor?(self.factory, i) };
            (!d.is_null()).then(|| {
                let d = unsafe { &*d };
                Descriptor { id: cstr(d.id), name: cstr(d.name), version: cstr(d.version) }
            })
        }).collect()
    }

    /// Make and `init` an instance of `id` (the first plugin if `None`).
    pub fn instantiate(self: &Arc<Self>, id: Option<&str>) -> Result<Instance> {
        let id = match id {
            Some(i) => i.to_owned(),
            None => self.descriptors().first().ok_or_else(|| anyhow!("bundle offers no plugins"))?.id.clone(),
        };
        let ctx = HostCtx::new();
        let cid = CString::new(id.clone())?;
        let f = unsafe { &*self.factory };
        let plugin = unsafe { (f.create_plugin.ok_or_else(|| anyhow!("no create_plugin"))?)(self.factory, &ctx.host, cid.as_ptr()) };
        if plugin.is_null() { bail!("create_plugin({id}) failed"); }
        if !unsafe { ((*plugin).init.ok_or_else(|| anyhow!("no init"))?)(plugin) } {
            unsafe { (*plugin).destroy.map(|d| d(plugin)) };
            bail!("{id}: init failed");
        }
        let handle = Arc::new(Handle { plugin, _ctx: ctx, _bundle: Arc::clone(self) });
        let mut inst = Instance { h: handle, active: false, params: Vec::new() };
        inst.params = inst.scan_params();
        Ok(inst)
    }
}

impl Drop for Bundle {
    fn drop(&mut self) {
        unsafe { (*self.entry).deinit.map(|d| d()) };
    }
}

// ── the host side the plugin sees ────────────────────────────────────────

struct HostCtx {
    host: clap_host,
    main: ThreadId,
    callback: AtomicBool,
}

static THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(host_is_main),
    is_audio_thread: Some(host_is_audio),
};

unsafe fn ctx_of<'a>(host: *const clap_host) -> &'a HostCtx { &*((*host).host_data as *const HostCtx) }
unsafe extern "C" fn host_is_main(h: *const clap_host) -> bool { ctx_of(h).main == std::thread::current().id() }
unsafe extern "C" fn host_is_audio(h: *const clap_host) -> bool { ctx_of(h).main != std::thread::current().id() }
unsafe extern "C" fn host_get_extension(_: *const clap_host, id: *const c_char) -> *const c_void {
    if CStr::from_ptr(id) == CLAP_EXT_THREAD_CHECK { &THREAD_CHECK as *const _ as *const c_void } else { ptr::null() }
}
unsafe extern "C" fn host_request_restart(_: *const clap_host) { log::debug!("clap: plugin asked for a restart (ignored)"); }
unsafe extern "C" fn host_request_process(_: *const clap_host) {}
unsafe extern "C" fn host_request_callback(h: *const clap_host) { ctx_of(h).callback.store(true, Ordering::Release); }

impl HostCtx {
    fn new() -> Box<HostCtx> {
        static NAME: &CStr = c"Overdeck";
        static VENDOR: &CStr = c"Audiodestrukt";
        static URL: &CStr = c"https://audiodestrukt.com";
        static VERSION: &CStr = c"0.1.0";
        let mut b = Box::new(HostCtx {
            host: clap_host {
                clap_version: CLAP_VERSION,
                host_data: ptr::null_mut(),
                name: NAME.as_ptr(), vendor: VENDOR.as_ptr(), url: URL.as_ptr(), version: VERSION.as_ptr(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            },
            main: std::thread::current().id(),
            callback: AtomicBool::new(false),
        });
        b.host.host_data = &*b as *const HostCtx as *mut c_void;
        b
    }
}

/// The plugin pointer, kept alive (with its host context and bundle) until
/// both halves let go.
struct Handle {
    plugin: *const clap_plugin,
    _ctx: Box<HostCtx>,
    _bundle: Arc<Bundle>,
}
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn ext<T>(&self, id: &CStr) -> Option<&T> {
        let p = unsafe { (*self.plugin).get_extension?(self.plugin, id.as_ptr()) } as *const T;
        (!p.is_null()).then(|| unsafe { &*p })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { (*self.plugin).destroy.map(|d| d(self.plugin)) };
    }
}

// ── the main-thread half ─────────────────────────────────────────────────

/// A parameter the plugin offers.
#[derive(Clone, Debug)]
pub struct Param {
    pub id: u32,
    pub name: String,
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

pub struct Instance {
    h: Arc<Handle>,
    active: bool,
    params: Vec<Param>,
}

impl Instance {
    fn scan_params(&self) -> Vec<Param> {
        let Some(p) = self.h.ext::<clap_plugin_params>(CLAP_EXT_PARAMS) else { return Vec::new() };
        let n = unsafe { p.count.map_or(0, |c| c(self.h.plugin)) };
        (0..n).filter_map(|i| {
            let mut info: clap_param_info = unsafe { std::mem::zeroed() };
            unsafe { p.get_info?(self.h.plugin, i, &mut info) }.then(|| Param {
                id: info.id,
                name: cstr(info.name.as_ptr()),
                module: cstr(info.module.as_ptr()),
                min: info.min_value, max: info.max_value, default: info.default_value,
            })
        }).collect()
    }

    pub fn params(&self) -> &[Param] { &self.params }

    /// A parameter by its display name (case-insensitive).
    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// The plugin's value for a parameter written as text (its own parsing:
    /// "6" for a Pitch of +6 %).  Main thread.
    pub fn text_to_value(&self, id: u32, text: &str) -> Option<f64> {
        let p = self.h.ext::<clap_plugin_params>(CLAP_EXT_PARAMS)?;
        let t = CString::new(text).ok()?;
        let mut v = 0.0;
        unsafe { p.text_to_value?(self.h.plugin, id, t.as_ptr(), &mut v) }.then_some(v)
    }

    pub fn save_state(&self) -> Result<Vec<u8>> {
        let s = self.h.ext::<clap_plugin_state>(CLAP_EXT_STATE).ok_or_else(|| anyhow!("no clap.state"))?;
        let mut out: Vec<u8> = Vec::new();
        unsafe extern "C" fn write(st: *const clap_ostream, buf: *const c_void, n: u64) -> i64 {
            let v = &mut *((*st).ctx as *mut Vec<u8>);
            v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, n as usize));
            n as i64
        }
        let os = clap_ostream { ctx: &mut out as *mut Vec<u8> as *mut c_void, write: Some(write) };
        if !unsafe { (s.save.ok_or_else(|| anyhow!("no save"))?)(self.h.plugin, &os) } { bail!("state save failed"); }
        Ok(out)
    }

    pub fn load_state(&self, bytes: &[u8]) -> Result<()> {
        let s = self.h.ext::<clap_plugin_state>(CLAP_EXT_STATE).ok_or_else(|| anyhow!("no clap.state"))?;
        struct Cur<'a> { b: &'a [u8], at: usize }
        let mut cur = Cur { b: bytes, at: 0 };
        unsafe extern "C" fn read(st: *const clap_istream, buf: *mut c_void, n: u64) -> i64 {
            let c = &mut *((*st).ctx as *mut Cur);
            let k = (n as usize).min(c.b.len() - c.at);
            ptr::copy_nonoverlapping(c.b[c.at..].as_ptr(), buf as *mut u8, k);
            c.at += k;
            k as i64
        }
        let is = clap_istream { ctx: &mut cur as *mut Cur as *mut c_void, read: Some(read) };
        if !unsafe { (s.load.ok_or_else(|| anyhow!("no load"))?)(self.h.plugin, &is) } { bail!("state load failed"); }
        Ok(())
    }

    pub fn activate(&mut self, sample_rate: f64, min_frames: u32, max_frames: u32) -> Result<()> {
        if self.active { return Ok(()); }
        if !unsafe { ((*self.h.plugin).activate.ok_or_else(|| anyhow!("no activate"))?)(self.h.plugin, sample_rate, min_frames, max_frames) } {
            bail!("activate failed");
        }
        self.active = true;
        Ok(())
    }

    /// Run the plugin's main-thread callback if it asked for one.  Call often
    /// (every few ms) from the main thread.
    pub fn idle(&self) {
        if self.h._ctx.callback.swap(false, Ordering::AcqRel) {
            unsafe { (*self.h.plugin).on_main_thread.map(|f| f(self.h.plugin)) };
        }
    }

    /// The audio-thread half.  Only one at a time.
    pub fn processor(&self) -> Processor {
        Processor { h: Arc::clone(&self.h), started: false, steady: 0, events: Vec::new(), outs: [Vec::new(), Vec::new()] }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if Arc::strong_count(&self.h) > 1 {
            log::warn!("clap: instance dropped while its processor lives; leaking it");
            std::mem::forget(Arc::clone(&self.h));
            return;
        }
        if self.active {
            unsafe { (*self.h.plugin).deactivate.map(|d| d(self.h.plugin)) };
        }
    }
}

// ── the audio-thread half ────────────────────────────────────────────────

/// A parameter change for the next block, at a frame within it.
#[derive(Clone, Copy, Debug)]
pub struct ParamEvent {
    pub frame: u32,
    pub id: u32,
    pub value: f64,
}

pub struct Processor {
    h: Arc<Handle>,
    started: bool,
    steady: i64,
    events: Vec<clap_event_param_value>,
    outs: [Vec<f32>; 2],
}

unsafe impl Send for Processor {}

unsafe extern "C" fn ev_size(l: *const clap_input_events) -> u32 { (*((*l).ctx as *const Vec<clap_event_param_value>)).len() as u32 }
unsafe extern "C" fn ev_get(l: *const clap_input_events, i: u32) -> *const clap_event_header {
    let v = &*((*l).ctx as *const Vec<clap_event_param_value>);
    v.get(i as usize).map_or(ptr::null(), |e| &e.header as *const clap_event_header)
}
unsafe extern "C" fn ev_push(_: *const clap_output_events, _: *const clap_event_header) -> bool { true }

impl Processor {
    /// Process one block into `left` / `right` (same length), with `events`
    /// (sorted by frame).  Starts processing on the first call.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], events: &[ParamEvent]) -> Result<()> {
        let frames = left.len().min(right.len());
        if !self.started {
            if !unsafe { ((*self.h.plugin).start_processing.ok_or_else(|| anyhow!("no start_processing"))?)(self.h.plugin) } {
                bail!("start_processing failed");
            }
            self.started = true;
        }
        self.events.clear();
        for e in events {
            self.events.push(clap_event_param_value {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_param_value>() as u32,
                    time: e.frame.min(frames.saturating_sub(1) as u32),
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_PARAM_VALUE,
                    flags: CLAP_EVENT_IS_LIVE,
                },
                param_id: e.id,
                cookie: ptr::null_mut(),
                note_id: -1, port_index: -1, channel: -1, key: -1,
                value: e.value,
            });
        }
        for o in &mut self.outs { o.resize(frames, 0.0); }
        let mut chans = [self.outs[0].as_mut_ptr(), self.outs[1].as_mut_ptr()];
        let mut out = clap_audio_buffer { data32: chans.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
        let ins = clap_input_events { ctx: &self.events as *const _ as *mut c_void, size: Some(ev_size), get: Some(ev_get) };
        let outs = clap_output_events { ctx: ptr::null_mut(), try_push: Some(ev_push) };
        let p = clap_process {
            steady_time: self.steady,
            frames_count: frames as u32,
            transport: ptr::null(),
            audio_inputs: ptr::null(), audio_outputs: &mut out,
            audio_inputs_count: 0, audio_outputs_count: 1,
            in_events: &ins, out_events: &outs,
        };
        let st = unsafe { ((*self.h.plugin).process.ok_or_else(|| anyhow!("no process"))?)(self.h.plugin, &p) };
        self.steady += frames as i64;
        if st == CLAP_PROCESS_ERROR { bail!("process error"); }
        left[..frames].copy_from_slice(&self.outs[0]);
        right[..frames].copy_from_slice(&self.outs[1]);
        Ok(())
    }
}

impl Drop for Processor {
    fn drop(&mut self) {
        if self.started { unsafe { (*self.h.plugin).stop_processing.map(|s| s(self.h.plugin)) }; }
    }
}
