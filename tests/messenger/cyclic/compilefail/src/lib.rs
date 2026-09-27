//! The two compile-fail pins of the messenger design, as `compile_fail`
//! doctests over the generated `interfaces` crate of the cyclic topology
//! (`rust_doc_test` in the BUILD file runs them).
//!
//! # A cycle without boxing does not compile
//!
//! The generated `messenger` crate boxes the route `a:Pong`. Written by
//! hand without the box, the two route futures contain each other and the
//! future type is infinitely sized:
//!
//! ```compile_fail,E0733
//! use std::sync::{Arc, Mutex};
//! use basable_core::{AppError, Ctx};
//! use basable_messenger::Route;
//! use interfaces::{source, AHandler, ASender, BHandler, BSender};
//! use messages::*;
//!
//! pub struct Naive {
//!     a: a::A,
//!     b: b::B,
//! }
//!
//! impl Route<Pong, Result<Count, AppError>, source::A, Ctx> for Naive {
//!     async fn route(&self, ctx: &Ctx, msg: Pong) -> Result<Count, AppError> {
//!         BHandler::<Naive>::handle_pong(&self.b, ctx, msg, BSender::new(self)).await
//!     }
//! }
//!
//! impl Route<Ping, Result<Count, AppError>, source::B, Ctx> for Naive {
//!     async fn route(&self, ctx: &Ctx, msg: Ping) -> Result<Count, AppError> {
//!         AHandler::<Naive>::handle_ping(&self.a, ctx, msg, ASender::new(self)).await
//!     }
//! }
//!
//! impl Route<Event, Result<(), AppError>, source::Api, Ctx> for Naive {
//!     async fn route(&self, _ctx: &Ctx, _msg: Event) -> Result<(), AppError> { Ok(()) }
//! }
//! impl Route<Ping, Result<Count, AppError>, source::Api, Ctx> for Naive {
//!     async fn route(&self, ctx: &Ctx, msg: Ping) -> Result<Count, AppError> {
//!         AHandler::<Naive>::handle_ping(&self.a, ctx, msg, ASender::new(self)).await
//!     }
//! }
//!
//! fn main() {
//!     let trace = Arc::new(Mutex::new(Vec::new()));
//!     let naive = Naive { a: a::A::new(trace.clone()), b: b::B::new(trace) };
//!     let ctx = Ctx::background();
//!     let _ = <Naive as Route<Ping, Result<Count, AppError>, source::Api, Ctx>>::route(&naive, &ctx, Ping { depth: 1 });
//! }
//! ```
//!
//! # A `std::sync::MutexGuard` across a send does not compile
//!
//! Every generated handler future is `Send`, and a `MutexGuard` is not, so
//! holding one across a `send_*` is refused (E0277, "future cannot be sent
//! between threads safely"), never a runtime hang:
//!
//! ```compile_fail,E0277
//! use std::sync::Mutex;
//! use basable_core::{AppError, Ctx};
//! use interfaces::{AHandler, ARoutes, ASender};
//! use messages::*;
//!
//! pub struct Holding {
//!     state: Mutex<u32>,
//! }
//!
//! impl<R: ARoutes> AHandler<R> for Holding {
//!     async fn handle_ping(&self, ctx: &Ctx, msg: Ping, s: ASender<'_, R>) -> Result<Count, AppError> {
//!         let guard = self.state.lock().unwrap();
//!         let below = s.send_pong(ctx, Pong { depth: msg.depth }).await?;
//!         drop(guard);
//!         Ok(below)
//!     }
//!
//!     async fn handle_event(&self, _ctx: &Ctx, _msg: Event, _s: ASender<'_, R>) -> Result<(), AppError> {
//!         Ok(())
//!     }
//! }
//! ```
//!
//! The same shape with the guard dropped before the send compiles:
//!
//! ```
//! use std::sync::Mutex;
//! use basable_core::{AppError, Ctx};
//! use interfaces::{AHandler, ARoutes, ASender};
//! use messages::*;
//!
//! pub struct Releasing {
//!     state: Mutex<u32>,
//! }
//!
//! impl<R: ARoutes> AHandler<R> for Releasing {
//!     async fn handle_ping(&self, ctx: &Ctx, msg: Ping, s: ASender<'_, R>) -> Result<Count, AppError> {
//!         let depth = {
//!             let mut guard = self.state.lock().unwrap();
//!             *guard += 1;
//!             msg.depth
//!         };
//!         s.send_pong(ctx, Pong { depth }).await
//!     }
//!
//!     async fn handle_event(&self, _ctx: &Ctx, _msg: Event, _s: ASender<'_, R>) -> Result<(), AppError> {
//!         Ok(())
//!     }
//! }
//! ```
