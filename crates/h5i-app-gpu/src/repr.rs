//! Optional, application-defined host/device conversion; ordinary kernels need
//! not implement this trait. No blanket cast or truncation is provided.

/// Fallible conversion at an application's CUDA boundary. Device types must
/// additionally satisfy the CUDA adapter's ABI/DeviceCopy requirements.
/// Capacity excess, invalid tags and unrepresentable values must be errors.
/// Applications may use separate input/output implementations and flatten
/// variable-length data into owned buffers rather than embedding host pointers.
pub trait DeviceRepr: Sized {
    type Device;
    type Error;
    fn to_device(&self) -> Result<Self::Device, Self::Error>;
    fn from_device(value: Self::Device) -> Result<Self, Self::Error>;
}
