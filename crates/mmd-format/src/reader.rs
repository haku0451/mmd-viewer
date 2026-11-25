//! Byte cursor with bounds checks.
//!
//! PMX index widths depend on the header, and every count is checked against
//! the remaining file before a buffer is allocated. An explicit cursor keeps
//! those checks next to the reads. See `docs/adr/0003-explicit-binary-cursor.md`.

use crate::error::FormatError;

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    /// Move the cursor back to a position previously returned by [`Self::position`].
    pub fn set_position(&mut self, pos: usize) -> Result<(), FormatError> {
        if pos > self.data.len() {
            return Err(FormatError::Truncated { context: "rewind" });
        }
        self.pos = pos;
        Ok(())
    }

    pub fn read_exact(
        &mut self,
        len: usize,
        context: &'static str,
    ) -> Result<&'a [u8], FormatError> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|end| *end <= self.data.len())
            .ok_or(FormatError::Truncated { context })?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self, context: &'static str) -> Result<u8, FormatError> {
        Ok(self.read_exact(1, context)?[0])
    }

    pub fn u16(&mut self, context: &'static str) -> Result<u16, FormatError> {
        let bytes = self.read_exact(2, context)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn u32(&mut self, context: &'static str) -> Result<u32, FormatError> {
        let bytes = self.read_exact(4, context)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn i32(&mut self, context: &'static str) -> Result<i32, FormatError> {
        Ok(self.u32(context)? as i32)
    }

    pub fn f32(&mut self, context: &'static str) -> Result<f32, FormatError> {
        let bytes = self.read_exact(4, context)?;
        let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(FormatError::NonFinite(context))
        }
    }

    pub fn vec3(&mut self, context: &'static str) -> Result<glam::Vec3, FormatError> {
        Ok(glam::Vec3::new(
            self.f32(context)?,
            self.f32(context)?,
            self.f32(context)?,
        ))
    }

    pub fn vec4(&mut self, context: &'static str) -> Result<glam::Vec4, FormatError> {
        Ok(glam::Vec4::new(
            self.f32(context)?,
            self.f32(context)?,
            self.f32(context)?,
            self.f32(context)?,
        ))
    }

    /// Read a count and reject it when `count * stride` cannot fit in the remainder.
    pub fn count(
        &mut self,
        context: &'static str,
        limit: u32,
        stride: usize,
    ) -> Result<u32, FormatError> {
        let count = self.u32(context)?;
        self.ensure_count(context, count, limit, stride)?;
        Ok(count)
    }

    pub fn ensure_count(
        &self,
        context: &'static str,
        count: u32,
        limit: u32,
        stride: usize,
    ) -> Result<(), FormatError> {
        if count > limit {
            return Err(FormatError::AllocationLimit {
                context,
                count: u64::from(count),
                limit: u64::from(limit),
            });
        }
        let bytes = (count as usize)
            .checked_mul(stride)
            .ok_or(FormatError::AllocationLimit {
                context,
                count: u64::from(count),
                limit: u64::from(limit),
            })?;
        if bytes > self.remaining() {
            return Err(FormatError::Truncated { context });
        }
        Ok(())
    }
}
