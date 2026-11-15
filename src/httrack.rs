//! HTTrack's plugin interface from `httrack-library.h` and `htsdefines.h`. This is the only module
//! that uses `unsafe`: its functions turn HTTrack's raw arguments into safe values, call the safe
//! functions in `callbacks`, and turn their results back into what HTTrack expects.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::marker::{PhantomData, PhantomPinned};
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::callbacks;

/// HTTrack's options, which HTTrack passes to `hts_plug` and to each callback.
#[derive(Clone, Copy)]
pub(crate) struct Options<'a> {
    raw: *mut HttrackOpt,
    lifetime: PhantomData<&'a HttrackOpt>,
}

impl Options<'_> {
    /// # Safety
    ///
    /// `raw` must be HTTrack's options, and they must stay valid while the plugin uses the result.
    const unsafe fn new(raw: *mut HttrackOpt) -> Self {
        Self {
            raw,
            lifetime: PhantomData,
        }
    }

    /// `None` when HTTrack does not know the extension.
    pub(crate) fn name_is_html(self, fil: &[u8]) -> Option<bool> {
        let fil = CString::new(fil).ok()?;
        // SAFETY: `self` holds HTTrack's options, and `fil` is a NUL-terminated string.
        match unsafe { is_knowntype(self.raw, fil.as_ptr()) } {
            0 => None,
            known => Some(known == 2),
        }
    }

    /// Whether HTTrack treats `mime` as unknown and guesses the type from the file name.
    pub(crate) fn may_unknown(self, mime: &CStr) -> bool {
        // SAFETY: `self` holds HTTrack's options, and `mime` is a NUL-terminated string.
        unsafe { may_unknown(self.raw, mime.as_ptr()) != 0 }
    }
}

/// A value that the plugin keeps between HTTrack's calls.
pub(crate) struct Global<T>(UnsafeCell<Option<Rc<T>>>);

// SAFETY: `htsdefines.h` says that HTTrack fires every callback that the plugin registers from the
// thread that called `hts_main2()`. `hts_plug` and `hts_unplug` require that no callback runs during
// them. So one thread at a time uses a `Global`.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    pub(crate) const fn new() -> Self {
        Self(UnsafeCell::new(None))
    }

    pub(crate) fn get(&self) -> Option<Rc<T>> {
        // SAFETY: one thread at a time uses `self`, and `get` and `set` call no code that uses
        // `self` while they hold a reference to its value.
        unsafe { (*self.0.get()).clone() }
    }

    pub(crate) fn set(&self, value: Option<T>) {
        // SAFETY: as in `get`. The old value drops after the reference to the cell ends.
        let old = unsafe { mem::replace(&mut *self.0.get(), value.map(Rc::new)) };
        drop(old);
    }
}

/// # Safety
///
/// `p` must be null or point to a NUL-terminated string that stays valid and unchanged for `'a`.
unsafe fn c_string<'a>(p: *const c_char) -> Option<&'a CStr> {
    // SAFETY: `p` is not null when `then` calls the closure, and the caller guarantees the rest.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) })
}

/// The bytes from `p` to the first `>` or NUL byte. `p` points into the page, so
/// `CStr::from_ptr` would read to its end.
///
/// # Safety
///
/// `p` must be null, or a `>` or a NUL byte must follow `p` in the same buffer. The buffer must
/// stay valid and unchanged for `'a`.
unsafe fn tag_bytes<'a>(p: *const c_char) -> Option<&'a [u8]> {
    if p.is_null() {
        return None;
    }
    let p = p.cast::<u8>();
    let mut len = 0;
    // SAFETY: the loop stops at the first `>` or NUL byte, which the caller guarantees is in the buffer.
    while !matches!(unsafe { *p.add(len) }, 0 | b'>') {
        len += 1;
    }
    // SAFETY: the `len` bytes from `p` precede the `>` or NUL byte, so they are in the buffer.
    Some(unsafe { std::slice::from_raw_parts(p, len) })
}

#[derive(Debug)]
#[repr(C)]
pub struct HttrackOpt {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

#[derive(Debug)]
#[repr(C)]
struct CallbackArg {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

// A Windows DLL must name the DLL it imports from.
#[cfg_attr(windows, link(name = "libhttrack", kind = "raw-dylib"))]
unsafe extern "C" {
    fn hts_malloc(size: usize) -> *mut c_void;
    fn hts_free(data: *mut c_void);
    fn htswrap_add(opt: *mut HttrackOpt, name: *const c_char, fct: *mut c_void) -> c_int;
    fn htswrap_read(opt: *mut HttrackOpt, name: *const c_char) -> usize;
    fn is_knowntype(opt: *mut HttrackOpt, fil: *const c_char) -> c_int;
    fn may_unknown(opt: *mut HttrackOpt, st: *const c_char) -> c_int;
}

type PreprocessFn = unsafe extern "C" fn(
    *mut CallbackArg,
    *mut HttrackOpt,
    *mut *mut c_char,
    *mut c_int,
    *const c_char,
    *const c_char,
) -> c_int;
type LinkDetected2Fn = unsafe extern "C" fn(*mut CallbackArg, *mut HttrackOpt, *mut c_char, *const c_char) -> c_int;
type CheckLinkFn =
    unsafe extern "C" fn(*mut CallbackArg, *mut HttrackOpt, *const c_char, *const c_char, c_int) -> c_int;
type CheckMimeFn = unsafe extern "C" fn(
    *mut CallbackArg,
    *mut HttrackOpt,
    *const c_char,
    *const c_char,
    *const c_char,
    c_int,
) -> c_int;
type ReceiveHeaderFn = unsafe extern "C" fn(
    *mut CallbackArg,
    *mut HttrackOpt,
    *mut c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
) -> c_int;
type EndFn = unsafe extern "C" fn(*mut CallbackArg, *mut HttrackOpt) -> c_int;

/// The callback that was in one of HTTrack's slots before this plugin's. The `carg` that
/// HTTrack passes belongs to it.
struct Previous<F> {
    address: AtomicUsize,
    callback_type: PhantomData<F>,
}

static PREV_PREPROCESS: Previous<PreprocessFn> = Previous::new();
static PREV_LINKDETECTED2: Previous<LinkDetected2Fn> = Previous::new();
static PREV_CHECK_LINK: Previous<CheckLinkFn> = Previous::new();
static PREV_CHECK_MIME: Previous<CheckMimeFn> = Previous::new();
static PREV_RECEIVE_HEADER: Previous<ReceiveHeaderFn> = Previous::new();
static PREV_END: Previous<EndFn> = Previous::new();

impl<F: Copy> Previous<F> {
    const fn new() -> Self {
        Self {
            address: AtomicUsize::new(0),
            callback_type: PhantomData,
        }
    }

    fn get(&self) -> Option<F> {
        const { assert!(size_of::<F>() == size_of::<usize>()) };
        let address = self.address.load(Ordering::Relaxed);
        // SAFETY: `register` stores only 0 or the address of a function of type `F`.
        (address != 0).then(|| unsafe { mem::transmute_copy::<usize, F>(&address) })
    }

    fn clear(&self) {
        self.address.store(0, Ordering::Relaxed);
    }
}

/// Puts `callback` in HTTrack's slot `name`, and keeps the callback that was there in `previous`.
/// Skips a slot that already holds `callback`, so that `previous` never points to it.
/// Returns `name` when HTTrack has no such slot.
///
/// # Safety
///
/// `F` must be the type of the function that HTTrack calls for `name`.
unsafe fn register<F: Copy>(
    opts: Options<'_>,
    name: &'static CStr,
    previous: &Previous<F>,
    callback: F,
) -> Result<(), &'static CStr> {
    const { assert!(size_of::<F>() == size_of::<*mut c_void>()) };
    // SAFETY: the caller guarantees that `F` is a function pointer type, which has the size of a pointer.
    let callback = unsafe { mem::transmute_copy::<F, *mut c_void>(&callback) };
    // SAFETY: `opts` holds HTTrack's options, and `name` is a NUL-terminated string.
    let current = unsafe { htswrap_read(opts.raw, name.as_ptr()) };
    if current == callback.addr() {
        return Ok(());
    }
    // The slot `name` holds 0 or a function of type `F`, as `Previous::get` requires.
    previous.address.store(current, Ordering::Relaxed);
    // SAFETY: `opts` holds HTTrack's options, `name` is a NUL-terminated string, and the caller
    // passes a `callback` of the type that HTTrack calls for `name`.
    if unsafe { htswrap_add(opts.raw, name.as_ptr(), callback) } == 0 {
        Ok(())
    } else {
        Err(name)
    }
}

/// Returns the name of a callback that HTTrack does not know.
pub(crate) fn register_callbacks(opts: Options<'_>) -> Result<(), &'static CStr> {
    // SAFETY: each callback has the type that HTTrack calls for its name.
    unsafe {
        register(opts, c"preprocess-html", &PREV_PREPROCESS, on_preprocess)?;
        register(opts, c"link-detected2", &PREV_LINKDETECTED2, on_linkdetected2)?;
        register(opts, c"check-link", &PREV_CHECK_LINK, on_check_link)?;
        register(opts, c"check-mime", &PREV_CHECK_MIME, on_check_mime)?;
        register(opts, c"receive-header", &PREV_RECEIVE_HEADER, on_receive_header)?;
        register(opts, c"end", &PREV_END, on_end)?;
    }
    Ok(())
}

/// Forgets the callbacks that `register_callbacks` replaced.
pub(crate) fn forget_previous_callbacks() {
    PREV_PREPROCESS.clear();
    PREV_LINKDETECTED2.clear();
    PREV_CHECK_LINK.clear();
    PREV_CHECK_MIME.clear();
    PREV_RECEIVE_HEADER.clear();
    PREV_END.clear();
}

/// `argv` is the plugin path and the comma-separated list paths.
///
/// # Safety
///
/// `opt` must be HTTrack's options and `argv` null or NUL-terminated. No callback
/// of this plugin may run during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hts_plug(opt: *mut HttrackOpt, argv: *const c_char) -> c_int {
    // SAFETY: the caller passes HTTrack's options, which stay valid during the call, and null or a
    // NUL-terminated `argv`.
    let (opts, argv) = unsafe { (Options::new(opt), c_string(argv)) };
    if callbacks::hts_plug(opts, argv).is_some() {
        1
    } else {
        0
    }
}

/// HTTrack calls it from `hts_free_opt` at the end of each mirror, and the
/// operating system can keep the plugin loaded for the next mirror.
///
/// # Safety
///
/// Any pointer is safe: the function does not read `_opt`. No callback of this
/// plugin may run during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hts_unplug(_opt: *mut HttrackOpt) -> c_int {
    callbacks::hts_unplug();
    1
}

unsafe extern "C" fn on_preprocess(
    carg: *mut CallbackArg,
    opt: *mut HttrackOpt,
    html: *mut *mut c_char,
    len: *mut c_int,
    adr: *const c_char,
    fil: *const c_char,
) -> c_int {
    // HTTrack takes the buffer only when the result is 1, and any other result
    // means "changed nothing".
    // SAFETY: HTTrack passed these arguments to call a `preprocess-html` callback.
    let prev_result = PREV_PREPROCESS
        .get()
        .map(|prev| unsafe { prev(carg, opt, html, len, adr, fil) });
    // SAFETY: HTTrack passes its options, null or NUL-terminated strings, and a page of `*len` bytes
    // at `*html`. They stay valid during the callback, and the page until `store_page` frees it.
    let (opts, adr, fil, input) = unsafe {
        let input = usize::try_from(*len)
            .ok()
            .map(|len| std::slice::from_raw_parts((*html).cast::<u8>(), len));
        (Options::new(opt), c_string(adr), c_string(fil), input)
    };
    let output = catch_unwind(AssertUnwindSafe(|| callbacks::on_preprocess(opts, adr, fil, input)))
        .ok()
        .flatten();
    // SAFETY: HTTrack passes valid pointers to the page and its length, and `output` does not borrow
    // from the page.
    let stored = output.is_some_and(|output| unsafe { store_page(html, len, &output) }.is_some());
    if stored { 1 } else { prev_result.unwrap_or(1) }
}

/// Replaces HTTrack's page with a copy of `page`. HTTrack frees the copy, so it comes from `hts_malloc`.
///
/// # Safety
///
/// `html` and `len` must be the pointers that HTTrack passes to `preprocess-html`.
unsafe fn store_page(html: *mut *mut c_char, len: *mut c_int, page: &[u8]) -> Option<()> {
    let page_len = c_int::try_from(page.len()).ok()?;
    // SAFETY: `hts_malloc` accepts any size.
    let buffer = unsafe { hts_malloc(page.len() + 1) }.cast::<u8>();
    if buffer.is_null() {
        return None;
    }
    // SAFETY: `buffer` is a new allocation of `page.len() + 1` bytes. For `preprocess-html`,
    // `htsdefines.h` says to `hts_free` the page in `*html` and that HTTrack frees the buffer
    // stored in its place.
    unsafe {
        ptr::copy_nonoverlapping(page.as_ptr(), buffer, page.len());
        *buffer.add(page.len()) = 0;
        hts_free((*html).cast());
        *html = buffer.cast();
        *len = page_len;
    }
    Some(())
}

/// Returns 0 to drop the link.
unsafe extern "C" fn on_linkdetected2(
    carg: *mut CallbackArg,
    opt: *mut HttrackOpt,
    link: *mut c_char,
    tag: *const c_char,
) -> c_int {
    if let Some(prev) = PREV_LINKDETECTED2.get()
        // SAFETY: HTTrack passed these arguments to call a `link-detected2` callback.
        && unsafe { prev(carg, opt, link, tag) } == 0
    {
        return 0;
    }
    // SAFETY: HTTrack passes null or a NUL-terminated link that stays valid during the callback. A
    // `tag` that is not null points into the page that HTTrack parses, which ends with a NUL byte, as
    // `htsdefines.h` says for `preprocess-html`.
    let (link, tag) = unsafe { (c_string(link), tag_bytes(tag)) };
    if callbacks::on_linkdetected2(link, tag) { 0 } else { 1 }
}

unsafe extern "C" fn on_check_link(
    carg: *mut CallbackArg,
    opt: *mut HttrackOpt,
    adr: *const c_char,
    fil: *const c_char,
    status: c_int,
) -> c_int {
    let verdict = match PREV_CHECK_LINK.get() {
        // SAFETY: HTTrack passed these arguments to call a `check-link` callback.
        Some(prev) => unsafe { prev(carg, opt, adr, fil, status) },
        None => -1,
    };
    // SAFETY: HTTrack passes null or NUL-terminated strings that stay valid during the callback.
    let (adr, fil) = unsafe { (c_string(adr), c_string(fil)) };
    let refused = verdict == 1 || (verdict == -1 && status == 1);
    if callbacks::on_check_link(adr, fil, refused) {
        1
    } else {
        verdict
    }
}

unsafe extern "C" fn on_receive_header(
    carg: *mut CallbackArg,
    opt: *mut HttrackOpt,
    buff: *mut c_char,
    adr: *const c_char,
    fil: *const c_char,
    referer_adr: *const c_char,
    referer_fil: *const c_char,
    incoming: *mut c_void,
) -> c_int {
    if let Some(prev) = PREV_RECEIVE_HEADER.get() {
        // SAFETY: HTTrack passed these arguments to call a `receive-header` callback.
        let verdict = unsafe { prev(carg, opt, buff, adr, fil, referer_adr, referer_fil, incoming) };
        if verdict != 1 {
            return verdict;
        }
    }
    // SAFETY: HTTrack passes null or NUL-terminated strings that stay valid during the callback.
    let (headers, adr, fil, referer_adr, referer_fil) = unsafe {
        (
            c_string(buff),
            c_string(adr),
            c_string(fil),
            c_string(referer_adr),
            c_string(referer_fil),
        )
    };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        callbacks::on_receive_header(headers, adr, fil, referer_adr, referer_fil)
    }));
    1
}

unsafe extern "C" fn on_check_mime(
    carg: *mut CallbackArg,
    opt: *mut HttrackOpt,
    adr: *const c_char,
    fil: *const c_char,
    mime: *const c_char,
    status: c_int,
) -> c_int {
    let verdict = match PREV_CHECK_MIME.get() {
        // SAFETY: HTTrack passed these arguments to call a `check-mime` callback.
        Some(prev) => unsafe { prev(carg, opt, adr, fil, mime, status) },
        None => -1,
    };
    let refused = verdict == 1 || (verdict == -1 && status == 1);
    // SAFETY: HTTrack passes its options and null or NUL-terminated strings, which stay valid during
    // the callback.
    let (opts, adr, fil, mime) = unsafe { (Options::new(opt), c_string(adr), c_string(fil), c_string(mime)) };
    if !refused {
        let _ = catch_unwind(AssertUnwindSafe(|| callbacks::on_check_mime(opts, adr, fil, mime)));
    }
    verdict
}

unsafe extern "C" fn on_end(carg: *mut CallbackArg, opt: *mut HttrackOpt) -> c_int {
    callbacks::on_end();
    match PREV_END.get() {
        // SAFETY: HTTrack passed these arguments to call an `end` callback.
        Some(prev) => unsafe { prev(carg, opt) },
        None => 1,
    }
}
