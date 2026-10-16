//! Small executor demonstration using the existing proved calculator kernel.
//! The read-only adapter delegates to the extracted, proved classification.
use calculator_kernel as k;
use h5i_app_core::{Kernel, TenantId};
use h5i_app_pg::{DbError, ReplyCodec, Store, Tx};
use std::sync::atomic::{AtomicU64, Ordering};
pub static LOADS: AtomicU64 = AtomicU64::new(0);
pub struct Calculator;
impl Kernel for Calculator {
    type Principal = k::Principal;
    type Snapshot = k::Snapshot;
    type Command = k::Command;
    type WriteSet = Option<k::Memory>;
    type Reply = k::Reply;
    type Error = k::Error;
    fn tenant(a: &k::Principal) -> TenantId { TenantId(a.org) }
    fn read_only(c: &k::Command) -> bool { k::read_only(c) }
    fn transition(a: &k::Principal, s: &k::Snapshot, c: &k::Command) -> Result<(Self::WriteSet, k::Reply), k::Error> { k::transition(a, s, c) }
    fn apply(s: &k::Snapshot, w: &Self::WriteSet) -> k::Snapshot { k::apply(s, w) }
}
pub struct CalculatorStore;
impl Store<Calculator> for CalculatorStore {
    fn ddl() -> Vec<String> { <crate::CalcStore as Store<crate::Calc>>::ddl() }
    fn tables() -> Vec<&'static str> { <crate::CalcStore as Store<crate::Calc>>::tables() }
    async fn load(tx: &Tx<'_>, t: TenantId) -> Result<k::Snapshot, DbError> {
        LOADS.fetch_add(1, Ordering::Relaxed);
        <crate::CalcStore as Store<crate::Calc>>::load(tx,t).await
    }
    async fn write(tx: &Tx<'_>, t: TenantId, w: &Option<k::Memory>) -> Result<(), DbError> { <crate::CalcStore as Store<crate::Calc>>::write(tx,t,w).await }
}
impl ReplyCodec<Calculator> for CalculatorStore {
    fn scope(a: &k::Principal) -> String { a.user.to_string() }
    fn fingerprint(c: &k::Command) -> Vec<u8> { format!("{c:?}").into_bytes() }
    fn encode(r: &k::Reply) -> Vec<u8> { let k::Reply::Value(v) = r; v.to_le_bytes().to_vec() }
    fn decode(b: &[u8]) -> Result<k::Reply,String> { Ok(k::Reply::Value(u64::from_le_bytes(b.try_into().map_err(|_| "invalid reply")?))) }
}
