use std::{mem, sync::Arc};

use crate::{
    multi_window::{MultiWindow, NewWindowRequest},
    windows::MyWindows,
};
use egui_glow::EguiGlow;
use glutin::{
    event::Event,
    event_loop::{ControlFlow, EventLoopWindowTarget},
    PossiblyCurrent,
};
use thiserror::Error;

/// A window being tracked by a `MultiWindow`. All tracked windows will be forwarded all events
/// received on the `MultiWindow`'s event loop.
#[enum_dispatch]
pub trait TrackedWindow {
    /// Returns true if the window is a root window. Root windows will close all other windows when closed
    fn is_root(&self) -> bool { false }

    /// Sets whether or not the window is a root window.
    fn set_root(&mut self, _root: bool) {}

    /// Handles one event from the event loop. Returns true if the window needs to be kept alive,
    /// otherwise it will be closed. Window events should be checked to ensure that their ID is one
    /// that the TrackedWindow is interested in.
    fn handle_event(
        &mut self,
        event: &glutin::event::Event<()>,
        other_windows: Vec<&mut MyWindows>,
        egui: &mut EguiGlow,
        gl_window: &mut glutin::WindowedContext<PossiblyCurrent>,
    ) -> TrackedWindowControl {
        // Child window's requested control flow.
        let mut control_flow = ControlFlow::Wait; // Unless this changes, we're fine waiting until the next event comes in.

        // Check if there are any root windows left, and exit if there are none.
        let mut root_window_exists = false;
        for other in &other_windows {
            if let MyWindows::Root(_) = other {
                root_window_exists = true;
            }
        }

        let mut redraw = || {
            let input = egui.egui_winit.take_egui_input(gl_window.window());
            let ppp = input.pixels_per_point;
            egui.egui_ctx.begin_frame(input);

            let quit = false;
            self.redraw(egui, gl_window);

            let full_output = egui.egui_ctx.end_frame();

            if quit {
                control_flow = glutin::event_loop::ControlFlow::Exit;
            } else if full_output.repaint_after.is_zero() {
                gl_window.window().request_redraw();
                control_flow = glutin::event_loop::ControlFlow::Poll;
            } else {
                control_flow = glutin::event_loop::ControlFlow::Wait;
            };

            {
                let color = egui::Rgba::from_rgb(0.1, 0.3, 0.2);
                unsafe {
                    use glow::HasContext as _;
                    egui.painter
                        .gl()
                        .clear_color(color[0], color[1], color[2], color[3]);
                    egui.painter.gl().clear(glow::COLOR_BUFFER_BIT);
                }

                // draw things behind egui here

                let prim = egui.egui_ctx.tessellate(full_output.shapes);
                egui.painter.paint_and_update_textures(
                    gl_window.window().inner_size().into(),
                    ppp.unwrap_or(1.0),
                    &prim[..],
                    &full_output.textures_delta,
                );

                // draw things on top of egui here

                gl_window.swap_buffers().unwrap();
            }
        };

        match event {
            // Platform-dependent event handlers to workaround a winit bug
            // See: https://github.com/rust-windowing/winit/issues/987
            // See: https://github.com/rust-windowing/winit/issues/1619
            glutin::event::Event::RedrawEventsCleared if cfg!(windows) => redraw(),
            glutin::event::Event::RedrawRequested(_) if !cfg!(windows) => redraw(),

            glutin::event::Event::WindowEvent { event, .. } => {
                if let glutin::event::WindowEvent::Resized(physical_size) = event {
                    gl_window.resize(*physical_size);
                }

                if let glutin::event::WindowEvent::CloseRequested = event {
                    control_flow = glutin::event_loop::ControlFlow::Exit;
                }

                egui.on_event(event);

                gl_window.window().request_redraw(); // TODO: ask egui if the events warrants a repaint instead
            }
            glutin::event::Event::LoopDestroyed => {
                egui.destroy();
            }

            _ => (),
        }

        if !root_window_exists && !self.is_root() {
            println!("Root window is gone, exiting popup.");
            control_flow = ControlFlow::Exit;
        }

        TrackedWindowControl {
            requested_control_flow: control_flow,
            windows_to_create: vec![],
        }
    }

    /// Runs the redraw for the window
    fn redraw(&mut self, egui: &mut EguiGlow,
        gl_window: &mut glutin::WindowedContext<PossiblyCurrent>);
}

pub struct TrackedWindowContainer {
    pub gl_window: IndeterminateWindowedContext,
    pub egui: Option<EguiGlow>,
    pub window: MyWindows,
}

impl TrackedWindowContainer {
    pub fn create<TE>(
        window: MyWindows,
        window_builder: glutin::window::WindowBuilder,
        event_loop: &glutin::event_loop::EventLoopWindowTarget<TE>,
    ) -> Result<TrackedWindowContainer, DisplayCreationError> {
        // let window_builder = glutin::window::WindowBuilder::new()
        //     .with_resizable(true)
        //     .with_inner_size(glutin::dpi::LogicalSize {
        //         width: 800.0,
        //         height: 600.0,
        //     })
        //     .with_title("egui_glow example");

        let gl_window = glutin::ContextBuilder::new()
            .with_depth_buffer(0)
            .with_srgb(true)
            .with_stencil_buffer(0)
            .with_vsync(true)
            .build_windowed(window_builder, event_loop)?;

        Ok(TrackedWindowContainer {
            window,
            gl_window: IndeterminateWindowedContext::NotCurrent(gl_window),
            egui: None,
        })
    }

    pub fn is_event_for_window(&self, event: &glutin::event::Event<()>) -> bool {
        // Check if the window ID matches, if not then this window can pass on the event.
        match (event, &self.gl_window) {
            (
                Event::WindowEvent {
                    window_id: id,
                    event: _,
                    ..
                },
                IndeterminateWindowedContext::PossiblyCurrent(gl_window),
            ) => gl_window.window().id() == *id,
            (
                Event::WindowEvent {
                    window_id: id,
                    event: _,
                    ..
                },
                IndeterminateWindowedContext::NotCurrent(gl_window),
            ) => gl_window.window().id() == *id,
            _ => true, // we weren't able to check the window ID, maybe this window is not initialized yet. we should run it.
        }
    }

    pub fn handle_event_outer<T>(
        &mut self,
        event: &glutin::event::Event<()>,
        el: &EventLoopWindowTarget<T>,
        other_windows: Vec<&mut crate::windows::MyWindows>,
    ) -> TrackedWindowControl {
        // Activate this gl_window so we can use it.
        // We cannot activate it without full ownership, so temporarily move the gl_window into the current scope.
        // It *must* be returned at the end.
        let gl_window = mem::replace(&mut self.gl_window, IndeterminateWindowedContext::None);
        let mut gl_window = match gl_window {
            IndeterminateWindowedContext::PossiblyCurrent(w) => unsafe {
                w.make_current().unwrap()
            },
            IndeterminateWindowedContext::NotCurrent(w) => unsafe { w.make_current().unwrap() },
            IndeterminateWindowedContext::None => panic!("there's no window context???"),
        };

        // Now that the window is active, create a context if it is missing.
        match self.egui.as_ref() {
            None => {
                let gl = Arc::new(unsafe {
                    glow::Context::from_loader_function(|s| gl_window.get_proc_address(s))
                });

                unsafe {
                    use glow::HasContext as _;
                    gl.enable(glow::FRAMEBUFFER_SRGB);
                }

                let egui = egui_glow::EguiGlow::new(&el, gl.clone());
                self.egui = Some(egui);
            }
            Some(_) => (),
            _ => {
                panic!("Partially initialized window");
            }
        };

        let result = match self.egui.as_mut() {
            Some(egui) => {
                let result = self
                    .window
                    .handle_event(event, other_windows, egui, &mut gl_window);
                if let ControlFlow::Exit = result.requested_control_flow {
                    // This window wants to go away. Close it.
                    egui.destroy();
                };
                result
            }
            _ => {
                panic!("Window wasn't fully initialized");
            }
        };

        match mem::replace(
            &mut self.gl_window,
            IndeterminateWindowedContext::PossiblyCurrent(gl_window),
        ) {
            IndeterminateWindowedContext::None => (),
            _ => {
                panic!("Window had a GL context while we were borrowing it?");
            }
        }
        result

        // self.gl_window.makecurr
        // We have to take ownership of it, because make_current()

        // let gl_window = mem::replace(&mut self.egui_window.gl_window, WindowedContext::None);
        // let gl_window = match gl_window {
        //     WindowedContext::PossiblyCurrent(w) => unsafe {w.make_current().unwrap()},
        //     WindowedContext::NotCurrent(w) => unsafe {w.make_current().unwrap()},
        //     WindowedContext::None => panic!("there's no window context???"),
        // };
    }
}

pub enum IndeterminateWindowedContext {
    PossiblyCurrent(glutin::WindowedContext<glutin::PossiblyCurrent>),
    NotCurrent(glutin::WindowedContext<glutin::NotCurrent>),
    None,
}

pub struct TrackedWindowControl {
    pub requested_control_flow: ControlFlow,
    pub windows_to_create: Vec<NewWindowRequest>,
}

#[derive(Error, Debug)]
pub enum DisplayCreationError {
    #[error("couldn't create window {0}")]
    Creation(#[from] glutin::CreationError),
    #[error("couldn't create context {0:?}")]
    Context(#[from] glutin::ContextError),
}
