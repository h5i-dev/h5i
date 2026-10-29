//! The `Kernel` an `[authz]` app runs: its transition behind the
//! authorization guard, called here rather than by the app.
//!
//! `h5i_app_std::authz::guard_viewed` is proven once, for every handler and
//! every policy (`h5i_app_std.Specs.authz`). [`Authz<T>`] is the only
//! `Kernel` impl for `Authz<_>` (coherence), and its `transition` *is* that
//! guard, with `T`'s `Transition`, `Policy` and `View` and `T::Reply`'s
//! `Disclose`. So whatever `T`'s handlers do, what the engine runs is
//! guarded: no per-app wiring decides it. The app chooses the types and the
//! tenant through [`AuthzKernel`], which `h5i_app_std::authz_kernel!` writes
//! in the kernel crate together with the `guarded_transition` the generated
//! `Authz.lean` proves, so the two instantiations are the same by
//! construction.

use std::marker::PhantomData;

use h5i_app_std::authz::{Disclose, Guarded, Policy, Transition, View, guard_viewed};

use crate::{Kernel, TenantId};

/// The types of a guarded kernel, and its tenant. Written by
/// `h5i_app_std::authz_kernel!` in the kernel crate (the orphan rule keeps
/// any other crate from implementing it for the kernel's type).
pub trait AuthzKernel: Send + Sync + 'static {
    type Principal: Send + Sync + Clone;
    type Snapshot: Send + Sync + Clone + Default;
    type Command: Send + Sync;
    type Write: Send + Sync + Clone;
    type Reply: Send + Sync + Disclose<Self::Row>;
    type Error: Send + Sync;
    /// A row a reply discloses.
    type Row;

    /// The only tenant this principal may read or write.
    fn tenant(actor: &Self::Principal) -> TenantId;
}

/// The kernel `T` behind the guard: the `Kernel` the server runs.
pub struct Authz<T>(PhantomData<fn() -> T>);

impl<T> Kernel for Authz<T>
where
    T: AuthzKernel
        + Transition<T::Principal, T::Snapshot, T::Command, T::Write, T::Reply, T::Error>
        + Policy<T::Principal, T::Snapshot, T::Write, T::Row>
        + View<T::Principal, T::Snapshot, T::Command>,
{
    type Principal = T::Principal;
    type Snapshot = T::Snapshot;
    type Command = T::Command;
    type WriteSet = Vec<T::Write>;
    type Reply = T::Reply;
    type Error = Guarded<T::Error>;

    fn tenant(actor: &T::Principal) -> TenantId {
        T::tenant(actor)
    }

    /// `guard_viewed` of `T`'s instances: `<kernel>.guarded_transition.authorized`.
    fn transition(
        actor: &T::Principal,
        snap: &T::Snapshot,
        cmd: &T::Command,
    ) -> Result<(Vec<T::Write>, T::Reply), Guarded<T::Error>> {
        guard_viewed::<T::Principal, T::Snapshot, T::Command, T::Write, T::Reply, T::Error, T::Row, T, T, T>(
            actor, snap, cmd,
        )
    }

    /// The kernel's `apply` (`<kernel>.guarded_transition.kernel_apply_eq`).
    fn apply(snap: &T::Snapshot, ws: &Vec<T::Write>) -> T::Snapshot {
        <T as Transition<T::Principal, T::Snapshot, T::Command, T::Write, T::Reply, T::Error>>::kernel_apply(snap, ws)
    }

    /// The view's `read_only`: `<kernel>.guarded_transition.read_only_sound`.
    fn read_only(cmd: &T::Command) -> bool {
        <T as View<T::Principal, T::Snapshot, T::Command>>::read_only(cmd)
    }
}
