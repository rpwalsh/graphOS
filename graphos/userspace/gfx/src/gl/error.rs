// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GL error type.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlError {
    /// Unacceptable value for an enumerated argument.
    InvalidEnum,
    /// Numeric argument is out of range.
    InvalidValue,
    /// Operation is illegal in current state.
    InvalidOperation,
    /// Framebuffer object is incomplete.
    InvalidFramebufferOperation,
    /// Kernel rejected the resource allocation (OOM).
    OutOfMemory,
    /// Named object does not exist.
    InvalidObject,
    /// Shader compilation failure.
    CompileFailed,
    /// Program link failure.
    LinkFailed,
}

impl core::fmt::Display for GlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidEnum => write!(f, "GL_INVALID_ENUM"),
            Self::InvalidValue => write!(f, "GL_INVALID_VALUE"),
            Self::InvalidOperation => write!(f, "GL_INVALID_OPERATION"),
            Self::InvalidFramebufferOperation => write!(f, "GL_INVALID_FRAMEBUFFER_OPERATION"),
            Self::OutOfMemory => write!(f, "GL_OUT_OF_MEMORY"),
            Self::InvalidObject => write!(f, "GL_INVALID_OBJECT"),
            Self::CompileFailed => write!(f, "GL_COMPILE_FAILED"),
            Self::LinkFailed => write!(f, "GL_LINK_FAILED"),
        }
    }
}
