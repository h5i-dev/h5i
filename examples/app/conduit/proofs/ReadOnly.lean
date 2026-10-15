import Theorems
/-!
# Read-only commands write nothing

The server declares `read_only` commands read-only (`Kernel::read_only`), so
`h5i-app-pg` runs them in a `SERIALIZABLE READ ONLY DEFERRABLE` transaction
without locks. `read_only_sound` is the obligation that makes this sound: for
every actor and state, an accepted read-only command returns no writes. The
reply theorems (`Replies.lean`) are stated for any snapshot, so they hold of
the consistent snapshot such a transaction reads.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Helpers
  conduit_kernel.Commands conduit_kernel.Theorems H5iAppLib

namespace conduit_kernel.ReadOnly

/-- In both variants, an accepted read-only command writes nothing. -/
theorem step_read_only (a : Principal) (s : Snapshot) (c : Command) (up : Bool) ws r
    (hro : read_only c = ok true) (h : step a s c up = .ok (.Ok (ws, r))) : ws.val = [] := by
  cases c <;> simp only [read_only, Result.ok.injEq, reduceCtorEq] at hro <;> simp only [step] at h
  case Login e p =>
    have := post_of_ok (login_spec s e p) h
    revert this; split <;> (try split) <;> intro h' <;> simp_all [ws_nil]
  case CurrentUser =>
    have := post_of_ok (current_user_spec s a.user) h
    revert this; split <;> intro h' <;> simp_all [ws_nil]
  case GetProfile n =>
    have := post_of_ok (get_profile_spec s a.user n) h
    revert this; split <;> intro h' <;> simp_all [ws_nil]
  case ListArticles tag au fav lim off =>
    obtain ⟨vs, hr, -⟩ := post_of_ok (list_articles_spec s a.user tag au fav lim off up) h
    simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr
    exact ws_nil hr.1
  case Feed lim off =>
    have := post_of_ok (feed_spec s a.user lim off up) h
    split at this
    · simp at this
    · obtain ⟨vs, hr, -⟩ := this
      simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr
      exact ws_nil hr.1
  case GetArticle slug =>
    have := post_of_ok (get_article_spec s a.user slug up) h
    split at this
    · simp at this
    · obtain ⟨vs, hr, -⟩ := this
      simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr
      exact ws_nil hr.1
  case GetComments slug =>
    have := post_of_ok (get_comments_spec s a.user slug) h
    split at this
    · simp at this
    · obtain ⟨vs, hr, -⟩ := this
      simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr
      exact ws_nil hr.1
  case GetTags =>
    obtain ⟨v, hv, -⟩ := (WP.spec_equiv_exists _ _).1 (all_tags_spec s.tags)
    rw [hv] at h; simp only [bind_ok, Result.ok.injEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    exact ws_nil h.1.symm

/-- The obligation of `Kernel::read_only`, for the kernel the server runs. -/
theorem read_only_sound : H5iAppLib.ReadOnly.Sound read_only transition (fun ws => ws.val) :=
  fun a s c ws r hro h => step_read_only a s c false ws r hro h

/-- And for upstream's variant. -/
theorem read_only_sound_upstream : H5iAppLib.ReadOnly.Sound read_only transition_upstream (fun ws => ws.val) :=
  fun a s c ws r hro h => step_read_only a s c true ws r hro h

end conduit_kernel.ReadOnly
