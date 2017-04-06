#![allow(non_camel_case_types)] // I prefer to use ALL_CAPS for type parameters
#![cfg_attr(test, feature(conservative_impl_trait))]

// If you're not compiling the unstable code, it often happens that
// there is stuff that is considered "dead code" and so forth. So
// disable warnings in that scenario.
#![cfg_attr(not(feature = "unstable"), allow(warnings))]

#[allow(unused_imports)]
use log::Event::*;
use std::any::Any;
use std::env;
use std::error::Error;
use std::str::FromStr;
use std::fmt;

extern crate deque;
#[macro_use]
extern crate lazy_static;
#[cfg(feature = "unstable")]
extern crate futures;
extern crate libc;
extern crate num_cpus;
extern crate rand;

#[macro_use]
mod log;

mod latch;
mod join;
mod job;
mod registry;
#[cfg(feature = "unstable")]
mod future;
mod scope;
mod sleep;
#[cfg(feature = "unstable")]
mod spawn_async;
mod test;
mod thread_pool;
mod unwind;
mod util;

pub use thread_pool::ThreadPool;
pub use join::join;
pub use scope::{scope, Scope};
#[cfg(feature = "unstable")]
pub use spawn_async::spawn_async;
#[cfg(feature = "unstable")]
pub use spawn_async::spawn_future_async;
#[cfg(feature = "unstable")]
pub use future::RayonFuture;

/// Returns the number of threads in the current registry. If this
/// code is executing within the Rayon thread-pool, then this will be
/// the number of threads for the current thread-pool. Otherwise, it
/// will be the number of threads for the global thread-pool.
///
/// This can be useful when trying to judge how many times to split
/// parallel work (the parallel iterator traits use this value
/// internally for this purpose).
pub fn current_num_threads() -> usize {
    ::registry::Registry::current_num_threads()
}

/// Contains the rayon thread pool configuration.
pub struct Configuration {
    /// The number of threads in the rayon thread pool.
    /// If zero will use the RAYON_RS_NUM_CPUS environment variable.
    /// If RAYON_RS_NUM_CPUS is invalid or zero will use the default.
    num_threads: usize,

    /// Custom closure, if any, to handle a panic that we cannot propagate
    /// anywhere else.
    panic_handler: Option<Box<PanicHandler>>,

    /// Closure to compute the name of a thread.
    get_thread_name: Option<Box<FnMut(usize) -> String>>,

    /// The stack size for the created worker threads
    stack_size: Option<usize>,

    /// Closure invoked on worker thread start.
    start_handler: Option<Box<StartHandler>>,

    /// Closure invoked on worker thread exit.
    exit_handler: Option<Box<ExitHandler>>,
}

/// The type for a panic handling closure. Note that this same closure
/// may be invoked multiple times in parallel.
type PanicHandler = Fn(Box<Any + Send>) + Send + Sync;

/// The type for a closure that gets invoked when a thread starts. The
/// closure is passed the index of the thread on which it is invoked.
/// Note that this same closure may be invoked multiple times in parallel.
type StartHandler = Fn(usize) + Send + Sync;

/// The type for a closure that gets invoked when a thread exits. The
/// closure is passed the index of the thread on which is is invoked.
/// Note that this same closure may be invoked multiple times in parallel.
type ExitHandler = Fn(usize) + Send + Sync;

impl Configuration {
    /// Creates and return a valid rayon thread pool configuration, but does not initialize it.
    pub fn new() -> Configuration {
        Configuration {
            num_threads: 0,
            get_thread_name: None,
            panic_handler: None,
            stack_size: None,
            start_handler: None,
            exit_handler: None,
        }
    }

    /// Get the number of threads that will be used for the thread
    /// pool. See `set_num_threads` for more information.
    fn num_threads(&self) -> usize {
        if self.num_threads > 0 {
            self.num_threads
        } else {
            match env::var("RAYON_RS_NUM_CPUS").ok().and_then(|s| usize::from_str(&s).ok()) {
                Some(x) if x > 0 => x,
                _ => num_cpus::get(),
            }
        }
    }

    /// Get the thread name for the thread with the given index.
    fn thread_name(&mut self, index: usize) -> Option<String> {
        self.get_thread_name.as_mut().map(|c| c(index))
    }

    /// Set a closure which takes a thread index and returns
    /// the thread's name.
    pub fn set_thread_name<F>(mut self, closure: F) -> Self
    where F: FnMut(usize) -> String + 'static {
        self.get_thread_name = Some(Box::new(closure));
        self
    }

    /// Set the number of threads to be used in the rayon threadpool.
    /// If `num_threads` is 0 or you do not call this function,
    /// rayon will use the RAYON_RS_NUM_CPUS environment variable.
    /// If RAYON_RS_NUM_CPUS is invalid or is zero, a suitable default will be used.
    /// Currently, the default is one thread per logical CPU.
    pub fn set_num_threads(mut self, num_threads: usize) -> Configuration {
        self.num_threads = num_threads;
        self
    }

    /// Returns a copy of the current panic handler.
    fn take_panic_handler(&mut self) -> Option<Box<PanicHandler>> {
        self.panic_handler.take()
    }

    /// Normally, whenever Rayon catches a panic, it tries to
    /// propagate it to someplace sensible, to try and reflect the
    /// semantics of sequential execution. But in some cases,
    /// particularly with the `spawn_async()` APIs, there is no
    /// obvious place where we should propagate the panic to.
    /// In that case, this panic handler is invoked.
    ///
    /// If no panic handler is set, the default is to abort the
    /// process, under the principle that panics should not go
    /// unobserved.
    ///
    /// If the panic handler itself panics, this will abort the
    /// process. To prevent this, wrap the body of your panic handler
    /// in a call to `std::panic::catch_unwind()`.
    pub fn set_panic_handler<H>(mut self, panic_handler: H) -> Configuration
        where H: Fn(Box<Any + Send>) + Send + Sync + 'static
    {
        self.panic_handler = Some(Box::new(panic_handler));
        self
    }

    /// Get the stack size of the worker threads
    fn stack_size(&self) -> Option<usize>{
        self.stack_size
    }

    /// Set the stack size of the worker threads
    pub fn set_stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = Some(stack_size);
        self
    }

    /// Returns a copy of the current thread start callback.
    fn take_start_handler(&mut self) -> Option<Box<StartHandler>> {
        self.start_handler.take()
    }

    /// Set a callback to be invoked on thread start.
    ///
    /// If this closure panics, the panic will be passed to the panic handler.
    /// If that handler returns, then startup will continue normally.
    pub fn set_start_handler<H>(mut self, start_handler: H) -> Configuration
        where H: Fn(usize) + Send + Sync + 'static
    {
        self.start_handler = Some(Box::new(start_handler));
        self
    }

    /// Returns a copy of the current thread exit callback.
    fn take_exit_handler(&mut self) -> Option<Box<ExitHandler>> {
        self.exit_handler.take()
    }

    /// Set a callback to be invoked on thread exit.
    ///
    /// If this closure panics, the panic will be passed to the panic handler.
    /// If that handler returns, then the thread will exit normally.
    pub fn set_exit_handler<H>(mut self, exit_handler: H) -> Configuration
        where H: Fn(usize) + Send + Sync + 'static
    {
        self.exit_handler = Some(Box::new(exit_handler));
        self
    }
}

/// Initializes the global thread pool. This initialization is
/// **optional**.  If you do not call this function, the thread pool
/// will be automatically initialized with the default
/// configuration. In fact, calling `initialize` is not recommended,
/// except for in two scenarios:
///
/// - You wish to change the default configuration.
/// - You are running a benchmark, in which case initializing may
///   yield slightly more consistent results, since the worker threads
///   will already be ready to go even in the first iteration.  But
///   this cost is minimal.
///
/// Initialization of the global thread pool happens exactly
/// once. Once started, the configuration cannot be
/// changed. Therefore, if you call `initialize` a second time, it
/// will return an error. An `Ok` result indicates that this
/// is the first initialization of the thread pool.
pub fn initialize(config: Configuration) -> Result<(), Box<Error>> {
    let registry = try!(registry::init_global_registry(config));
    registry.wait_until_primed();
    Ok(())
}

/// This is a debugging API not really intended for end users. It will
/// dump some performance statistics out using `println`.
#[cfg(feature = "unstable")]
pub fn dump_stats() {
    dump_stats!();
}

impl fmt::Debug for Configuration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let Configuration { ref num_threads, ref get_thread_name, ref panic_handler, ref stack_size,
                            ref start_handler, ref exit_handler } = *self;

        // Just print `Some("<closure>")` or `None` to the debug
        // output.
        let get_thread_name = get_thread_name.as_ref().map(|_| "<closure>");

        // Just print `Some("<closure>")` or `None` to the debug
        // output.
        let panic_handler = panic_handler.as_ref().map(|_| "<closure>");
        let start_handler = start_handler.as_ref().map(|_| "<closure>");
        let exit_handler = exit_handler.as_ref().map(|_| "<closure>");

        f.debug_struct("Configuration")
         .field("num_threads", num_threads)
         .field("get_thread_name", &get_thread_name)
         .field("panic_handler", &panic_handler)
         .field("stack_size", &stack_size)
         .field("start_handler", &start_handler)
         .field("exit_handler", &exit_handler)
         .finish()
    }
}
