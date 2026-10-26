//! The authorization guard: a kernel's writes and disclosed rows, checked
//! against a policy after `transition` returns.
//!
//! An app implements [`Transition`] (its `transition` and `apply`), [`Policy`]
//! (which writes and rows each actor may make and see) and [`Disclose`] (the
//! rows a reply contains), then runs commands through [`guard`]. The guard's
//! spec, `h5i_app_std.Specs.authz.guard_authorized` in `proofs/AuthzSpecs.lean`,
//! never looks inside `transition`: it holds for every kernel, so an app gets
//! its authorization theorem by instantiating it (`docs/design/design-authz.md`).
//!
//! Write `i` is checked in the state `apply(s, ws[..i])`, the kernel's own
//! commit semantics, so a command may create a row and then act on it. A
//! disclosed row must be visible before the command or after it. Fail closed:
//! one refused write or row refuses the whole command, and nothing commits.

/// A command's refusal: the kernel's own, or the guard's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guarded<E> {
    App(E),
    /// A write the policy does not permit, or a row it does not let the actor see.
    Denied,
}

/// A kernel's `transition` and `apply`, as in `h5i_app_core::Kernel`. The
/// methods are prefixed so an impl can delegate to the kernel's own
/// `transition` and `apply`: in the extracted Lean, a method named like the
/// function it calls would resolve to itself.
pub trait Transition<A, S, C, W, R, E> {
    fn kernel_transition(actor: &A, s: &S, cmd: &C) -> Result<(Vec<W>, R), E>;
    /// Commit semantics of a write set: the function the store must agree with.
    fn kernel_apply(s: &S, ws: &Vec<W>) -> S;
}

/// What each actor may write and see. `D` is the type of disclosed rows,
/// usually an enum with one variant per table.
pub trait Policy<A, S, W, D> {
    /// May `actor` make write `w` in state `s`?
    fn permits(actor: &A, s: &S, w: &W) -> bool;
    /// May `actor` see row `d` in state `s`?
    fn visible(actor: &A, s: &S, d: &D) -> bool;
}

/// The rows a reply contains. Every row the reply carries must be listed:
/// a row left out is a row the guard does not check.
pub trait Disclose<D> {
    fn disclosed(&self) -> Vec<D>;
}

/// The first `n` writes, cloned.
pub fn prefix<W: Clone>(ws: &Vec<W>, n: usize) -> Vec<W> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < n && i < ws.len() {
        out.push(ws[i].clone());
        i += 1;
    }
    out
}

/// Every write is permitted in the state the writes before it leave.
pub fn check_writes<A, S, C, W: Clone, R, E, D, T: Transition<A, S, C, W, R, E>, P: Policy<A, S, W, D>>(
    actor: &A,
    s: &S,
    ws: &Vec<W>,
) -> bool {
    let mut i = 0;
    while i < ws.len() {
        let cur = T::kernel_apply(s, &prefix(ws, i));
        if !P::permits(actor, &cur, &ws[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// Every row is visible in `before` or in `after`.
pub fn check_reads<A, S, W, D, P: Policy<A, S, W, D>>(actor: &A, before: &S, after: &S, ds: &Vec<D>) -> bool {
    let mut i = 0;
    while i < ds.len() {
        if !P::visible(actor, before, &ds[i]) && !P::visible(actor, after, &ds[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// `T::transition`, refused with [`Guarded::Denied`] unless every write is
/// permitted and every disclosed row is visible.
pub fn guard<A, S, C, W: Clone, R: Disclose<D>, E, D, T: Transition<A, S, C, W, R, E>, P: Policy<A, S, W, D>>(
    actor: &A,
    s: &S,
    cmd: &C,
) -> Result<(Vec<W>, R), Guarded<E>> {
    match T::kernel_transition(actor, s, cmd) {
        Err(e) => Err(Guarded::App(e)),
        Ok(out) => {
            if !check_writes::<A, S, C, W, R, E, D, T, P>(actor, s, &out.0) {
                return Err(Guarded::Denied);
            }
            let after = T::kernel_apply(s, &out.0);
            if !check_reads::<A, S, W, D, P>(actor, s, &after, &out.1.disclosed()) {
                return Err(Guarded::Denied);
            }
            Ok(out)
        }
    }
}

/// Read-only commands and the part of a state an actor may see. With
/// [`guard_viewed`], a read-only command runs on `view(actor, s)` only, so its
/// reply, derived values included, cannot depend on anything the view leaves
/// out (`h5i_app_std.Specs.authz.guard_viewed_noninterference`).
pub trait View<A, S, C> {
    /// `cmd` never writes. A read-only command that returns writes is denied.
    fn read_only(cmd: &C) -> bool;
    /// The state `actor` may see: the rows the policy lets it read.
    fn view(actor: &A, s: &S) -> S;
}

/// [`guard`] for commands that write. A read-only command runs on the
/// actor's view: it is denied if it returns writes or discloses a row not
/// visible in the view, and nothing else of `s` reaches its result.
pub fn guard_viewed<
    A,
    S,
    C,
    W: Clone,
    R: Disclose<D>,
    E,
    D,
    T: Transition<A, S, C, W, R, E>,
    P: Policy<A, S, W, D>,
    V: View<A, S, C>,
>(
    actor: &A,
    s: &S,
    cmd: &C,
) -> Result<(Vec<W>, R), Guarded<E>> {
    if !V::read_only(cmd) {
        return guard::<A, S, C, W, R, E, D, T, P>(actor, s, cmd);
    }
    let v = V::view(actor, s);
    match T::kernel_transition(actor, &v, cmd) {
        Err(e) => Err(Guarded::App(e)),
        Ok(out) => {
            if out.0.len() != 0 {
                return Err(Guarded::Denied);
            }
            if !check_reads::<A, S, W, D, P>(actor, &v, &v, &out.1.disclosed()) {
                return Err(Guarded::Denied);
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A counter only actor 1 may raise, and only by one at a time.
    struct Counter;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Set(u64);

    struct Seen(Vec<u64>);

    impl Disclose<u64> for Seen {
        fn disclosed(&self) -> Vec<u64> {
            self.0.clone()
        }
    }

    impl Transition<u64, u64, Vec<u64>, Set, Seen, ()> for Counter {
        fn kernel_transition(_: &u64, s: &u64, cmd: &Vec<u64>) -> Result<(Vec<Set>, Seen), ()> {
            Ok((cmd.iter().map(|&v| Set(v)).collect(), Seen(vec![*s])))
        }
        fn kernel_apply(s: &u64, ws: &Vec<Set>) -> u64 {
            ws.last().map(|w| w.0).unwrap_or(*s)
        }
    }

    impl Policy<u64, u64, Set, u64> for Counter {
        fn permits(actor: &u64, s: &u64, w: &Set) -> bool {
            *actor == 1 && w.0 == *s + 1
        }
        fn visible(_: &u64, s: &u64, d: &u64) -> bool {
            d == s
        }
    }

    fn run(actor: u64, s: u64, cmd: Vec<u64>) -> Result<(Vec<Set>, Seen), Guarded<()>> {
        guard::<u64, u64, Vec<u64>, Set, Seen, (), u64, Counter, Counter>(&actor, &s, &cmd)
    }

    #[test]
    fn each_write_sees_the_ones_before() {
        assert!(run(1, 0, vec![1, 2, 3]).is_ok());
        assert_eq!(run(1, 0, vec![1, 3]).err(), Some(Guarded::Denied));
        assert_eq!(run(2, 0, vec![1]).err(), Some(Guarded::Denied));
        assert!(run(2, 0, vec![]).is_ok());
    }

    /// Commands of length 0 read; the view is 0 for actors other than 1.
    impl View<u64, u64, Vec<u64>> for Counter {
        fn read_only(cmd: &Vec<u64>) -> bool {
            cmd.is_empty()
        }
        fn view(actor: &u64, s: &u64) -> u64 {
            if *actor == 1 { *s } else { 0 }
        }
    }

    fn read(actor: u64, s: u64) -> Result<(Vec<Set>, Seen), Guarded<()>> {
        guard_viewed::<u64, u64, Vec<u64>, Set, Seen, (), u64, Counter, Counter, Counter>(&actor, &s, &vec![])
    }

    #[test]
    fn reads_see_only_the_view() {
        assert_eq!(read(1, 7).unwrap().1.0, vec![7]);
        assert_eq!(read(2, 7).unwrap().1.0, vec![0]);
        assert_eq!(read(2, 7).unwrap().1.0, read(2, 9).unwrap().1.0);
    }

    #[test]
    fn prefix_stops_at_the_end() {
        assert_eq!(prefix(&vec![1, 2, 3], 2), vec![1, 2]);
        assert_eq!(prefix(&vec![1, 2, 3], 9), vec![1, 2, 3]);
    }
}
