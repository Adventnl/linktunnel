//! Windows 10 1809+ synthetic touch and pen. Devices are created lazily on the input thread,
//! contacts are submitted as complete frames, and all contacts are canceled on disconnect.
use crate::{InputError, TouchPhase};
use std::collections::BTreeMap;
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE,
    POINTER_FEEDBACK_NONE, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows::Win32::UI::Input::Pointer::*;
use windows::Win32::UI::WindowsAndMessaging::{
    PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_TILT_X,
    PEN_MASK_TILT_Y, POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH, TOUCH_MASK_CONTACTAREA,
    TOUCH_MASK_PRESSURE,
};

#[derive(Debug)]
struct Device(HSYNTHETICPOINTERDEVICE);
// SAFETY: the input worker owns this device; moving an unused injector to that worker is safe.
unsafe impl Send for Device {}
impl Device {
    fn new(kind: POINTER_INPUT_TYPE, count: u32) -> Result<Self, InputError> {
        // SAFETY: valid pointer type, documented count limits and feedback mode.
        unsafe { CreateSyntheticPointerDevice(kind, count, POINTER_FEEDBACK_NONE) }
            .map(Self)
            .map_err(failed)
    }
    fn inject(&self, frame: &[POINTER_TYPE_INFO]) -> Result<(), InputError> {
        // SAFETY: device and initialized pointer frame are live and match the device type.
        unsafe { InjectSyntheticPointerInput(self.0, frame) }.map_err(failed)
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the handle belongs to this value and is destroyed exactly once.
        unsafe { DestroySyntheticPointerDevice(self.0) };
    }
}
fn failed(e: impl std::fmt::Display) -> InputError {
    InputError::Failed(format!("Windows pointer injection: {e}"))
}
fn flags(phase: TouchPhase) -> POINTER_FLAGS {
    match phase {
        TouchPhase::Down => POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
        TouchPhase::Move => POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
        TouchPhase::Up => POINTER_FLAG_UP,
        TouchPhase::Cancel => POINTER_FLAG_UP | POINTER_FLAG_CANCELED,
    }
}
fn info(kind: POINTER_INPUT_TYPE, id: u8, phase: TouchPhase, x: i32, y: i32) -> POINTER_INFO {
    POINTER_INFO {
        pointerType: kind,
        pointerId: u32::from(id) + 1,
        pointerFlags: flags(phase),
        ptPixelLocation: POINT { x, y },
        ..Default::default()
    }
}
fn touch_frame(contact: POINTER_TOUCH_INFO) -> POINTER_TYPE_INFO {
    POINTER_TYPE_INFO {
        r#type: PT_TOUCH,
        Anonymous: POINTER_TYPE_INFO_0 { touchInfo: contact },
    }
}
fn pen_frame(contact: POINTER_PEN_INFO) -> POINTER_TYPE_INFO {
    POINTER_TYPE_INFO {
        r#type: PT_PEN,
        Anonymous: POINTER_TYPE_INFO_0 { penInfo: contact },
    }
}

#[derive(Debug, Default)]
pub(super) struct Pointers {
    touch_device: Option<Device>,
    pen_device: Option<Device>,
    touch: BTreeMap<u8, POINTER_TOUCH_INFO>,
    pen: Option<(u8, POINTER_PEN_INFO)>,
}
// SAFETY: devices are owned handles and all operations require exclusive access. Contact
// records are initialized locally by `info`; their sourceDevice/hwndTarget fields stay null
// and are never dereferenced. Moving this state to the input worker shares no OS pointers.
unsafe impl Send for Pointers {}
impl Pointers {
    pub(super) fn touch(
        &mut self,
        id: u8,
        phase: TouchPhase,
        x: i32,
        y: i32,
        pressure: u16,
    ) -> Result<(), InputError> {
        let exists = self.touch.contains_key(&id);
        if (phase == TouchPhase::Down && exists) || (phase != TouchPhase::Down && !exists) {
            return Err(InputError::Invalid(
                "invalid touch contact transition".into(),
            ));
        }
        if phase == TouchPhase::Down && self.touch.len() >= 10 {
            return Err(InputError::Invalid(
                "at most ten touch contacts are supported".into(),
            ));
        }
        if self.touch_device.is_none() {
            self.touch_device = Some(Device::new(PT_TOUCH, 10)?);
        }
        let contact = POINTER_TOUCH_INFO {
            pointerInfo: info(PT_TOUCH, id, phase, x, y),
            touchMask: TOUCH_MASK_CONTACTAREA | TOUCH_MASK_PRESSURE,
            rcContact: RECT {
                left: x.saturating_sub(2),
                top: y.saturating_sub(2),
                right: x.saturating_add(2),
                bottom: y.saturating_add(2),
            },
            pressure: u32::from(pressure) * 1024 / 65535,
            ..Default::default()
        };
        let mut frame: Vec<POINTER_TYPE_INFO> = self
            .touch
            .iter()
            .filter(|(other, _)| **other != id)
            .map(|(_, contact)| {
                let mut contact = *contact;
                contact.pointerInfo.pointerFlags = flags(TouchPhase::Move);
                touch_frame(contact)
            })
            .collect();
        frame.push(touch_frame(contact));
        self.touch_device
            .as_ref()
            .ok_or_else(|| failed("missing touch device"))?
            .inject(&frame)?;
        if matches!(phase, TouchPhase::Up | TouchPhase::Cancel) {
            self.touch.remove(&id);
        } else {
            self.touch.insert(id, contact);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn pen(
        &mut self,
        id: u8,
        phase: TouchPhase,
        x: i32,
        y: i32,
        pressure: u16,
        tilt_x: i8,
        tilt_y: i8,
        buttons: u8,
    ) -> Result<(), InputError> {
        if (phase == TouchPhase::Down && self.pen.is_some())
            || (phase != TouchPhase::Down
                && self.pen.as_ref().is_none_or(|(active, _)| *active != id))
        {
            return Err(InputError::Invalid("invalid pen contact transition".into()));
        }
        if self.pen_device.is_none() {
            self.pen_device = Some(Device::new(PT_PEN, 1)?);
        }
        let mut pen_flags = 0;
        if buttons & 1 != 0 {
            pen_flags |= PEN_FLAG_BARREL;
        }
        if buttons & 2 != 0 {
            pen_flags |= PEN_FLAG_ERASER | PEN_FLAG_INVERTED;
        }
        let contact = POINTER_PEN_INFO {
            pointerInfo: info(PT_PEN, id, phase, x, y),
            penFlags: pen_flags,
            penMask: PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
            pressure: u32::from(pressure) * 1024 / 65535,
            tiltX: i32::from(tilt_x).clamp(-90, 90),
            tiltY: i32::from(tilt_y).clamp(-90, 90),
            ..Default::default()
        };
        self.pen_device
            .as_ref()
            .ok_or_else(|| failed("missing pen device"))?
            .inject(&[pen_frame(contact)])?;
        self.pen = if matches!(phase, TouchPhase::Up | TouchPhase::Cancel) {
            None
        } else {
            Some((id, contact))
        };
        Ok(())
    }

    pub(super) fn release_all(&mut self) -> Result<(), InputError> {
        let mut first = None;
        if !self.touch.is_empty() {
            let frame: Vec<_> = self
                .touch
                .values()
                .map(|contact| {
                    let mut contact = *contact;
                    contact.pointerInfo.pointerFlags = flags(TouchPhase::Cancel);
                    touch_frame(contact)
                })
                .collect();
            if let Some(device) = &self.touch_device
                && let Err(e) = device.inject(&frame)
            {
                first.get_or_insert(e);
            }
            self.touch.clear();
        }
        if let Some((_, mut contact)) = self.pen.take() {
            contact.pointerInfo.pointerFlags = flags(TouchPhase::Cancel);
            if let Some(device) = &self.pen_device
                && let Err(e) = device.inject(&[pen_frame(contact)])
            {
                first.get_or_insert(e);
            }
        }
        // Destroying devices additionally ends contacts when the desktop disappeared and a
        // cancellation frame was refused. The next event creates a device on the new desktop.
        self.touch_device = None;
        self.pen_device = None;
        first.map_or(Ok(()), Err)
    }
}
impl Drop for Pointers {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}
