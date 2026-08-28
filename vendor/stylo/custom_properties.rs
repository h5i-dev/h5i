/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Support for [custom properties for cascading variables][custom].
//!
//! [custom]: https://drafts.csswg.org/css-variables/

use crate::applicable_declarations::{CascadePriority, RevertKind};
use crate::computed_value_flags::ComputedValueFlags;
use crate::custom_properties_map::{AllSubstitutionFunctions, CustomPropertiesMap, OwnMap};
use crate::device::Device;
use crate::dom::AttributeTracker;
use crate::properties::{
    CSSWideKeyword, CustomDeclaration, CustomDeclarationValue, LonghandId, LonghandIdSet,
    PropertyDeclaration,
};
use crate::properties_and_values::{
    rule::Descriptors as PropertyDescriptors,
    syntax::{data_type::DependentDataTypes, Descriptor as SyntaxDescriptor},
    value::{
        AllowComputationallyDependent, ComputedValue as ComputedRegisteredValue,
        SpecifiedValue as SpecifiedRegisteredValue,
    },
};
use crate::selector_map::{PrecomputedHashMap, PrecomputedHashSet};
use crate::stylesheets::UrlExtraData;
use crate::stylist::Stylist;
use crate::typed_om::{
    ToTyped, TypedValue, UnparsedSegment, UnparsedValue, VariableReferenceValue,
};
use crate::values::computed::{self, ToComputedValue};
use crate::values::generics::calc::SortKey as AttrUnit;
use crate::values::specified::NoCalcLength;
use crate::values::specified::ParsedNamespace;
use crate::{derives::*, Namespace, Prefix};
use crate::{Atom, LocalName};
use cssparser::{
    CowRcStr, Delimiter, Parser, ParserInput, SourcePosition, Token, TokenSerializationType,
};
use rustc_hash::FxHashMap;
use selectors::parser::SelectorParseErrorKind;
use servo_arc::Arc;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::fmt::{self, Write};
use std::ops::{Index, IndexMut};
use std::{cmp, num};
use style_traits::{CssString, CssWriter, ParseError, StyleParseErrorKind, ToCss};
use thin_vec::ThinVec;

/// The environment from which to get `env` function values.
///
/// TODO(emilio): If this becomes a bit more complex we should probably move it
/// to the `media_queries` module, or something.
#[derive(Debug, MallocSizeOf)]
pub struct CssEnvironment;

type EnvironmentEvaluator = fn(device: &Device, url_data: &UrlExtraData) -> VariableValue;

struct EnvironmentVariable {
    name: Atom,
    evaluator: EnvironmentEvaluator,
}

macro_rules! make_variable {
    ($name:expr, $evaluator:expr) => {{
        EnvironmentVariable {
            name: $name,
            evaluator: $evaluator,
        }
    }};
}

fn get_safearea_inset_top(device: &Device, url_data: &UrlExtraData) -> VariableValue {
    VariableValue::pixels(device.safe_area_insets().top, url_data)
}

fn get_safearea_inset_bottom(device: &Device, url_data: &UrlExtraData) -> VariableValue {
    VariableValue::pixels(device.safe_area_insets().bottom, url_data)
}

fn get_safearea_inset_left(device: &Device, url_data: &UrlExtraData) -> VariableValue {
    VariableValue::pixels(device.safe_area_insets().left, url_data)
}

fn get_safearea_inset_right(device: &Device, url_data: &UrlExtraData) -> VariableValue {
    VariableValue::pixels(device.safe_area_insets().right, url_data)
}

#[cfg(feature = "gecko")]
fn get_content_preferred_color_scheme(device: &Device, url_data: &UrlExtraData) -> VariableValue {
    use crate::queries::values::PrefersColorScheme;
    let prefers_color_scheme = unsafe {
        crate::gecko_bindings::bindings::Gecko_MediaFeatures_PrefersColorScheme(
            device.document(),
            /* use_content = */ true,
        )
    };
    VariableValue::ident(
        match prefers_color_scheme {
            PrefersColorScheme::Light => "light",
            PrefersColorScheme::Dark => "dark",
        },
        url_data,
    )
}

#[cfg(feature = "servo")]
fn get_content_preferred_color_scheme(_device: &Device, url_data: &UrlExtraData) -> VariableValue {
    // TODO: Add an implementation for Servo.
    VariableValue::ident("light", url_data)
}

fn get_scrollbar_inline_size(device: &Device, url_data: &UrlExtraData) -> VariableValue {
    VariableValue::pixels(device.scrollbar_inline_size().px(), url_data)
}

fn get_hairline(device: &Device, url_data: &UrlExtraData) -> VariableValue {
    VariableValue::pixels(
        app_units::Au(device.app_units_per_device_pixel()).to_f32_px(),
        url_data,
    )
}

static ENVIRONMENT_VARIABLES: [EnvironmentVariable; 4] = [
    make_variable!(atom!("safe-area-inset-top"), get_safearea_inset_top),
    make_variable!(atom!("safe-area-inset-bottom"), get_safearea_inset_bottom),
    make_variable!(atom!("safe-area-inset-left"), get_safearea_inset_left),
    make_variable!(atom!("safe-area-inset-right"), get_safearea_inset_right),
];

#[cfg(feature = "gecko")]
macro_rules! lnf_int {
    ($id:ident) => {
        unsafe {
            crate::gecko_bindings::bindings::Gecko_GetLookAndFeelInt(
                crate::gecko_bindings::bindings::LookAndFeel_IntID::$id as i32,
            )
        }
    };
}

#[cfg(feature = "servo")]
macro_rules! lnf_int {
    ($id:ident) => {
        // TODO: Add an implementation for Servo.
        0
    };
}

macro_rules! lnf_int_variable {
    ($atom:expr, $id:ident, $ctor:ident) => {{
        fn __eval(_: &Device, url_data: &UrlExtraData) -> VariableValue {
            VariableValue::$ctor(lnf_int!($id), url_data)
        }
        make_variable!($atom, __eval)
    }};
}

fn eval_gtk_csd_titlebar_radius(device: &Device, url_data: &UrlExtraData) -> VariableValue {
    let int_pixels = lnf_int!(TitlebarRadius);
    let unzoomed_scale =
        device.device_pixel_ratio_ignoring_full_zoom().get() / device.device_pixel_ratio().get();
    VariableValue::pixels(int_pixels as f32 * unzoomed_scale, url_data)
}

static CHROME_ENVIRONMENT_VARIABLES: [EnvironmentVariable; 9] = [
    make_variable!(
        atom!("-moz-gtk-csd-titlebar-radius"),
        eval_gtk_csd_titlebar_radius
    ),
    lnf_int_variable!(
        atom!("-moz-gtk-csd-tooltip-radius"),
        TooltipRadius,
        int_pixels
    ),
    lnf_int_variable!(
        atom!("-moz-gtk-csd-close-button-position"),
        GTKCSDCloseButtonPosition,
        integer
    ),
    lnf_int_variable!(
        atom!("-moz-gtk-csd-minimize-button-position"),
        GTKCSDMinimizeButtonPosition,
        integer
    ),
    lnf_int_variable!(
        atom!("-moz-gtk-csd-maximize-button-position"),
        GTKCSDMaximizeButtonPosition,
        integer
    ),
    lnf_int_variable!(
        atom!("-moz-overlay-scrollbar-fade-duration"),
        ScrollbarFadeDuration,
        int_ms
    ),
    make_variable!(
        atom!("-moz-content-preferred-color-scheme"),
        get_content_preferred_color_scheme
    ),
    make_variable!(atom!("scrollbar-inline-size"), get_scrollbar_inline_size),
    make_variable!(atom!("hairline"), get_hairline),
];

impl CssEnvironment {
    #[inline]
    fn get(&self, name: &Atom, device: &Device, url_data: &UrlExtraData) -> Option<VariableValue> {
        if let Some(var) = ENVIRONMENT_VARIABLES.iter().find(|var| var.name == *name) {
            return Some((var.evaluator)(device, url_data));
        }
        if !url_data.chrome_rules_enabled() {
            return None;
        }
        let var = CHROME_ENVIRONMENT_VARIABLES
            .iter()
            .find(|var| var.name == *name)?;
        Some((var.evaluator)(device, url_data))
    }
}

/// A custom property name is just an `Atom`.
///
/// Note that this does not include the `--` prefix
pub type Name = Atom;

/// Parse a custom property name.
///
/// <https://drafts.csswg.org/css-variables/#typedef-custom-property-name>
pub fn parse_name(s: &str) -> Result<&str, ()> {
    if s.starts_with("--") && s.len() > 2 {
        Ok(&s[2..])
    } else {
        Err(())
    }
}

/// A value for a custom property is just a set of tokens.
///
/// We preserve the original CSS for serialization, and also the variable
/// references to other custom property names.
#[derive(Clone, Debug, MallocSizeOf, ToShmem)]
pub struct VariableValue {
    /// The raw CSS string.
    pub css: String,

    /// The url data of the stylesheet where this value came from.
    pub url_data: UrlExtraData,

    first_token_type: TokenSerializationType,
    last_token_type: TokenSerializationType,

    /// var(), env(), attr() or non-custom property (e.g. through `em`) references.
    references: References,
}

trivial_to_computed_value!(VariableValue);

/// Given a potentially registered variable value turn it into a computed custom property value.
pub fn compute_variable_value(
    value: &Arc<VariableValue>,
    registration: &PropertyDescriptors,
    computed_context: &computed::Context,
) -> Option<ComputedRegisteredValue> {
    if registration.is_universal() {
        return Some(ComputedRegisteredValue::universal(Arc::clone(value)));
    }
    compute_value(
        &value.css,
        &value.url_data,
        registration,
        computed_context,
        AttrTaint::default(),
    )
    .ok()
}

// For all purposes, we want values to be considered equal if their css text is equal.
impl PartialEq for VariableValue {
    fn eq(&self, other: &Self) -> bool {
        self.css == other.css
    }
}

impl Eq for VariableValue {}

impl ToCss for SpecifiedValue {
    fn to_css<W>(&self, dest: &mut CssWriter<W>) -> fmt::Result
    where
        W: Write,
    {
        dest.write_str(&self.css)
    }
}

impl ToTyped for SpecifiedValue {
    fn to_typed(&self, dest: &mut ThinVec<TypedValue>) -> Result<(), ()> {
        let unparsed_value = reify_variable_value(self)?;
        dest.push(TypedValue::Unparsed(unparsed_value));
        Ok(())
    }
}

fn reify_variable_value(value: &VariableValue) -> Result<UnparsedValue, ()> {
    let mut reference_index = 0;
    reify_variable_value_range(
        &value.css,
        &value.references.refs,
        &mut reference_index,
        0,
        value.css.len(),
    )
}

/// Reify a slice of the CSS string into UnparsedSegment entries.
///
/// References are stored in source order, with outer substitution functions
/// inserted before references in their fallback. The shared `reference_index`
/// relies on this ordering to recurse into fallbacks without reprocessing
/// nested referecences.
fn reify_variable_value_range(
    css: &str,
    references: &[SubstitutionFunctionReference],
    reference_index: &mut usize,
    start: usize,
    end: usize,
) -> Result<UnparsedValue, ()> {
    debug_assert!(start <= end);
    debug_assert!(end <= css.len());

    let mut values = ThinVec::new();
    let mut cur_pos = start;

    while *reference_index < references.len() {
        let reference = &references[*reference_index];

        if reference.start >= end {
            break;
        }

        debug_assert!(reference.start >= cur_pos);
        debug_assert!(reference.start <= reference.end);
        debug_assert!(reference.end <= css.len());

        if cur_pos < reference.start {
            values.push(UnparsedSegment::String(CssString::from(
                &css[cur_pos..reference.start],
            )));
        }

        *reference_index += 1;

        if reference.substitution_kind != SubstitutionFunctionKind::Var {
            return Err(());
        }

        let (fallback, has_fallback) = if let Some(fallback) = &reference.fallback {
            debug_assert!(fallback.start.get() <= reference.end - 1);

            (
                reify_variable_value_range(
                    css,
                    references,
                    reference_index,
                    fallback.start.get(),
                    reference.end - 1, // Skip the closing ')'.
                )?,
                true,
            )
        } else {
            (ThinVec::new(), false)
        };

        values.push(UnparsedSegment::VariableReference(VariableReferenceValue {
            variable: CssString::from(format!("--{}", reference.name)),
            fallback,
            has_fallback,
        }));

        cur_pos = reference.end;
    }

    if cur_pos < end {
        values.push(UnparsedSegment::String(CssString::from(&css[cur_pos..end])));
    }

    Ok(values)
}

/// A pair of separate CustomPropertiesMaps, split between custom properties
/// that have the inherit flag set and those with the flag unset.
#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComputedCustomProperties {
    /// Map for custom properties with inherit flag set, including non-registered
    /// ones.
    pub inherited: CustomPropertiesMap,
    /// Map for custom properties with inherit flag unset.
    pub non_inherited: CustomPropertiesMap,
}

impl ComputedCustomProperties {
    /// Return whether the inherited and non_inherited maps are none.
    pub fn is_empty(&self) -> bool {
        self.inherited.is_empty() && self.non_inherited.is_empty()
    }

    /// Return the name and value of the property at specified index, if any.
    pub fn property_at(&self, index: usize) -> Option<(&Name, &Option<ComputedRegisteredValue>)> {
        // Just expose the custom property items from custom_properties.inherited, followed
        // by custom property items from custom_properties.non_inherited.
        self.inherited
            .get_index(index)
            .or_else(|| self.non_inherited.get_index(index - self.inherited.len()))
    }

    /// Insert a custom property in the corresponding inherited/non_inherited
    /// map, depending on whether the inherit flag is set or unset.
    pub(crate) fn insert(
        &mut self,
        registration: &PropertyDescriptors,
        name: &Name,
        value: ComputedRegisteredValue,
    ) {
        self.map_mut(registration).insert(name, value)
    }

    /// Remove a custom property from the corresponding inherited/non_inherited
    /// map, depending on whether the inherit flag is set or unset.
    pub(crate) fn remove(&mut self, registration: &PropertyDescriptors, name: &Name) {
        self.map_mut(registration).remove(name);
    }

    /// Shrink the capacity of the inherited maps as much as possible.
    fn shrink_to_fit(&mut self) {
        self.inherited.shrink_to_fit();
        self.non_inherited.shrink_to_fit();
    }

    fn map_mut(&mut self, registration: &PropertyDescriptors) -> &mut CustomPropertiesMap {
        if registration.inherits() {
            &mut self.inherited
        } else {
            &mut self.non_inherited
        }
    }

    /// Returns the relevant custom property value given a registration.
    pub fn get(
        &self,
        registration: &PropertyDescriptors,
        name: &Name,
    ) -> Option<&ComputedRegisteredValue> {
        if registration.inherits() {
            self.inherited.get(name)
        } else {
            self.non_inherited.get(name)
        }
    }
}

/// Both specified and computed values are VariableValues, the difference is
/// whether var() functions are expanded.
pub type SpecifiedValue = VariableValue;
/// Both specified and computed values are VariableValues, the difference is
/// whether var() functions are expanded.
pub type ComputedValue = VariableValue;

/// Set of flags to non-custom references this custom property makes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, MallocSizeOf, ToShmem)]
struct NonCustomReferences(u8);

bitflags! {
    impl NonCustomReferences: u8 {
        /// At least one custom property depends on font-relative units.
        const FONT_UNITS = 1 << 0;
        /// At least one custom property depends on root element's font-relative units.
        const ROOT_FONT_UNITS = 1 << 1;
        /// At least one custom property depends on line height units.
        const LH_UNITS = 1 << 2;
        /// At least one custom property depends on root element's line height units.
        const ROOT_LH_UNITS = 1 << 3;
        /// All dependencies not depending on the root element.
        const NON_ROOT_DEPENDENCIES = Self::FONT_UNITS.0 | Self::LH_UNITS.0;
        /// All dependencies depending on the root element.
        const ROOT_DEPENDENCIES = Self::ROOT_FONT_UNITS.0 | Self::ROOT_LH_UNITS.0;
    }
}

impl NonCustomReferences {
    fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(SingleNonCustomReference),
    {
        for (_, r) in self.iter_names() {
            let single = match r {
                Self::FONT_UNITS => SingleNonCustomReference::FontUnits,
                Self::ROOT_FONT_UNITS => SingleNonCustomReference::RootFontUnits,
                Self::LH_UNITS => SingleNonCustomReference::LhUnits,
                Self::ROOT_LH_UNITS => SingleNonCustomReference::RootLhUnits,
                _ => unreachable!("Unexpected single bit value"),
            };
            f(single);
        }
    }

    fn from_unit(value: &CowRcStr) -> Self {
        // For registered properties, any reference to font-relative dimensions
        // make it dependent on font-related properties.
        // TODO(dshin): When we unit algebra gets implemented and handled -
        // Is it valid to say that `calc(1em / 2em * 3px)` triggers this?
        if value.eq_ignore_ascii_case(NoCalcLength::LH) {
            return Self::FONT_UNITS | Self::LH_UNITS;
        }
        if value.eq_ignore_ascii_case(NoCalcLength::EM)
            || value.eq_ignore_ascii_case(NoCalcLength::EX)
            || value.eq_ignore_ascii_case(NoCalcLength::CAP)
            || value.eq_ignore_ascii_case(NoCalcLength::CH)
            || value.eq_ignore_ascii_case(NoCalcLength::IC)
        {
            return Self::FONT_UNITS;
        }
        if value.eq_ignore_ascii_case(NoCalcLength::RLH) {
            return Self::ROOT_FONT_UNITS | Self::ROOT_LH_UNITS;
        }
        if value.eq_ignore_ascii_case(NoCalcLength::REM)
            || value.eq_ignore_ascii_case(NoCalcLength::REX)
            || value.eq_ignore_ascii_case(NoCalcLength::RCH)
            || value.eq_ignore_ascii_case(NoCalcLength::RCAP)
            || value.eq_ignore_ascii_case(NoCalcLength::RIC)
        {
            return Self::ROOT_FONT_UNITS;
        }
        Self::empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SingleNonCustomReference {
    FontUnits = 0,
    RootFontUnits,
    LhUnits,
    RootLhUnits,
}

struct NonCustomReferenceMap<T>([Option<T>; 4]);

impl<T> Default for NonCustomReferenceMap<T> {
    fn default() -> Self {
        NonCustomReferenceMap(Default::default())
    }
}

impl<T> Index<SingleNonCustomReference> for NonCustomReferenceMap<T> {
    type Output = Option<T>;

    fn index(&self, reference: SingleNonCustomReference) -> &Self::Output {
        &self.0[reference as usize]
    }
}

impl<T> IndexMut<SingleNonCustomReference> for NonCustomReferenceMap<T> {
    fn index_mut(&mut self, reference: SingleNonCustomReference) -> &mut Self::Output {
        &mut self.0[reference as usize]
    }
}

/// Whether to defer resolving custom properties referencing font relative units.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum DeferFontRelativeCustomPropertyResolution {
    Yes,
    No,
}

/// Substitution function source: var, env, attr.
#[derive(Copy, Clone, Debug, MallocSizeOf, Hash, Eq, PartialEq, ToShmem, Parse)]
pub enum SubstitutionFunctionKind {
    /// CSS variable / custom property
    Var,
    /// Environment variable
    Env,
    /// DOM attribute
    Attr,
}

/// A wrapper map that encapsulates both the custom properties and attributes
/// for a given element.
#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComputedSubstitutionFunctions {
    /// The applicable custom properties (includes inherited and non-inherited).
    pub custom_properties: ComputedCustomProperties,
    /// The applicable DOM attributes.
    pub attributes: OwnMap,
}

impl ComputedSubstitutionFunctions {
    /// Creates a substitution function map from optional custom properties
    /// and DOM attributes.
    #[inline(always)]
    pub fn new(
        custom_properties: Option<ComputedCustomProperties>,
        attributes: Option<OwnMap>,
    ) -> Self {
        Self {
            custom_properties: custom_properties.unwrap_or_default(),
            attributes: attributes.unwrap_or_default(),
        }
    }

    #[inline(always)]
    fn insert_var(
        &mut self,
        registration: &PropertyDescriptors,
        name: &Name,
        value: ComputedRegisteredValue,
    ) {
        self.custom_properties.insert(registration, name, value);
    }

    #[inline(always)]
    fn insert_attr(&mut self, name: &Name, value: ComputedRegisteredValue) {
        self.attributes.insert(name.clone(), Some(value));
    }

    #[inline(always)]
    fn remove_var(&mut self, registration: &PropertyDescriptors, name: &Name) {
        self.custom_properties.remove(registration, name);
    }

    #[inline(always)]
    fn remove_attr(&mut self, name: &Name) {
        self.attributes.insert(name.clone(), None);
    }

    #[inline(always)]
    fn get_var(
        &self,
        registration: &PropertyDescriptors,
        name: &Name,
    ) -> Option<&ComputedRegisteredValue> {
        self.custom_properties.get(registration, name)
    }

    #[inline(always)]
    fn get_attr(&self, name: &Name) -> Option<&ComputedRegisteredValue> {
        self.attributes.get(name).and_then(|p| p.as_ref())
    }
}

#[derive(Clone, Debug, MallocSizeOf, PartialEq, ToShmem, Parse)]
enum AttributeType {
    Invalid,
    None,
    RawString,
    Type(SyntaxDescriptor),
    Unit(AttrUnit),
}

#[derive(Clone, Debug, MallocSizeOf, PartialEq, ToShmem)]
struct AttributeData {
    kind: AttributeType,
    namespace: ParsedNamespace,
}

/// For a CSS string, the range, counted in bytes, that is attr()-tainted.
#[derive(Clone, Debug, Default, MallocSizeOf, PartialEq, ToShmem, ToComputedValue)]
pub struct AttrTaintedRange {
    /// Start of the range, counted in bytes. Inclusive.
    start: usize,
    /// End of the range, counted in bytes. Exclusive.
    end: usize,
}

impl AttrTaintedRange {
    /// Creates a range within a CSS string that is tainted by attr().
    #[inline(always)]
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end);
        Self { start, end }
    }
}

/// In CSS Values and Units, values produced by `attr()` are considered attr()-tainted, as are
/// functions that contain an attr()-tainted value. Using an attr()-tainted value as or in a <url>
/// makes a declaration invalid at computed-value time.
/// https://drafts.csswg.org/css-values-5/#attr-security
#[derive(Clone, Debug, Default, MallocSizeOf, PartialEq, ToShmem)]
pub struct AttrTaint(SmallVec<[AttrTaintedRange; 1]>);

impl AttrTaint {
    /// For a CSS string, determine whether any `<url>` overlapping this `range`
    /// is disallowed due to attr()-tainting.
    #[inline(always)]
    pub fn should_disallow_urls_in_range(&self, range: &AttrTaintedRange) -> bool {
        self.0
            .iter()
            .any(|r| r.start <= range.end && r.end >= range.start)
    }

    /// Returns true if the attr()-tainted range contains no elements.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[inline(always)]
    fn new_fully_tainted(end: usize) -> Self {
        let mut taint = Self::default();
        taint.push(0, end);
        taint
    }

    #[inline(always)]
    fn push(&mut self, start: usize, end: usize) {
        self.0.push(AttrTaintedRange::new(start, end));
    }
}

#[derive(Clone, Debug, MallocSizeOf, PartialEq, ToShmem)]
struct VariableFallback {
    // NOTE(emilio): We don't track fallback end, because we rely on the missing closing
    // parenthesis, if any, to be inserted, which means that we can rely on our end being
    // reference.end - 1.
    start: num::NonZeroUsize,
    first_token_type: TokenSerializationType,
    last_token_type: TokenSerializationType,
}

#[derive(Clone, Debug, MallocSizeOf, PartialEq, ToShmem)]
struct SubstitutionFunctionReference {
    name: Name,
    start: usize,
    end: usize,
    fallback: Option<VariableFallback>,
    attribute_data: AttributeData,
    prev_token_type: TokenSerializationType,
    next_token_type: TokenSerializationType,
    substitution_kind: SubstitutionFunctionKind,
}

/// A struct holding information about the external references to that a custom
/// property value may have.
#[derive(Clone, Debug, Default, MallocSizeOf, PartialEq, ToShmem)]
struct References {
    refs: Vec<SubstitutionFunctionReference>,
    non_custom_references: NonCustomReferences,
    any_env: bool,
    any_var: bool,
    any_attr: bool,
}

impl References {
    fn has_references(&self) -> bool {
        !self.refs.is_empty()
    }

    fn non_custom_references(&self, is_root_element: bool) -> NonCustomReferences {
        let mut mask = NonCustomReferences::NON_ROOT_DEPENDENCIES;
        if is_root_element {
            mask |= NonCustomReferences::ROOT_DEPENDENCIES
        }
        self.non_custom_references & mask
    }
}

impl VariableValue {
    fn empty(url_data: &UrlExtraData) -> Self {
        Self {
            css: String::new(),
            last_token_type: Default::default(),
            first_token_type: Default::default(),
            url_data: url_data.clone(),
            references: Default::default(),
        }
    }

    /// Create a new custom property without parsing if the CSS is known to be valid and contain no
    /// references.
    pub fn new(
        css: String,
        url_data: &UrlExtraData,
        first_token_type: TokenSerializationType,
        last_token_type: TokenSerializationType,
    ) -> Self {
        Self {
            css,
            url_data: url_data.clone(),
            first_token_type,
            last_token_type,
            references: References::default(),
        }
    }

    fn push<'i>(
        &mut self,
        css: &str,
        css_first_token_type: TokenSerializationType,
        css_last_token_type: TokenSerializationType,
        attr_taint: Option<&mut AttrTaint>,
    ) -> Result<(), ()> {
        /// Prevent values from getting terribly big since you can use custom
        /// properties exponentially.
        ///
        /// This number (2MB) is somewhat arbitrary, but silly enough that no
        /// reasonable page should hit it. We could limit by number of total
        /// substitutions, but that was very easy to work around in practice
        /// (just choose a larger initial value and boom).
        const MAX_VALUE_LENGTH_IN_BYTES: usize = 2 * 1024 * 1024;

        if self.css.len() + css.len() > MAX_VALUE_LENGTH_IN_BYTES {
            return Err(());
        }

        // This happens e.g. between two subsequent var() functions:
        // `var(--a)var(--b)`.
        //
        // In that case, css_*_token_type is nonsensical.
        if css.is_empty() {
            return Ok(());
        }

        self.first_token_type.set_if_nothing(css_first_token_type);
        // If self.first_token_type was nothing,
        // self.last_token_type is also nothing and this will be false:
        if self
            .last_token_type
            .needs_separator_when_before(css_first_token_type)
        {
            self.css.push_str("/**/")
        }
        let start = self.css.len();
        self.css.push_str(css);
        let end = self.css.len();
        if let Some(taint) = attr_taint {
            taint.push(start, end);
        }
        self.last_token_type = css_last_token_type;
        Ok(())
    }

    /// Parse a custom property value.
    pub fn parse<'i, 't>(
        input: &mut Parser<'i, 't>,
        namespaces: Option<&FxHashMap<Prefix, Namespace>>,
        url_data: &UrlExtraData,
    ) -> Result<Self, ParseError<'i>> {
        let mut references = References::default();
        let mut missing_closing_characters = String::new();
        let start_position = input.position();
        let (first_token_type, last_token_type) = parse_declaration_value(
            input,
            start_position,
            namespaces,
            &mut references,
            &mut missing_closing_characters,
        )?;
        let mut css = input
            .slice_from(start_position)
            .trim_ascii_start()
            .to_owned();
        if !missing_closing_characters.is_empty() {
            // Unescaped backslash at EOF in a quoted string is ignored.
            if css.ends_with("\\")
                && matches!(missing_closing_characters.as_bytes()[0], b'"' | b'\'')
            {
                css.pop();
            }
            css.push_str(&missing_closing_characters);
        }

        css.truncate(css.trim_ascii_end().len());
        css.shrink_to_fit();
        references.refs.shrink_to_fit();

        Ok(Self {
            css,
            url_data: url_data.clone(),
            first_token_type,
            last_token_type,
            references,
        })
    }

    /// Returns whether this value is tainted by `attr()`.
    pub fn is_attr_tainted(&self) -> bool {
        self.references.any_attr
    }

    /// Create VariableValue from an int.
    fn integer(number: i32, url_data: &UrlExtraData) -> Self {
        Self::from_token(
            Token::Number {
                has_sign: false,
                value: number as f32,
                int_value: Some(number),
            },
            url_data,
        )
    }

    /// Create VariableValue from an int.
    fn ident(ident: &'static str, url_data: &UrlExtraData) -> Self {
        Self::from_token(Token::Ident(ident.into()), url_data)
    }

    /// Create VariableValue from a float amount of CSS pixels.
    fn pixels(number: f32, url_data: &UrlExtraData) -> Self {
        // FIXME (https://github.com/servo/rust-cssparser/issues/266):
        // No way to get TokenSerializationType::Dimension without creating
        // Token object.
        Self::from_token(
            Token::Dimension {
                has_sign: false,
                value: number,
                int_value: None,
                unit: CowRcStr::from("px"),
            },
            url_data,
        )
    }

    /// Create VariableValue from an integer amount of milliseconds.
    fn int_ms(number: i32, url_data: &UrlExtraData) -> Self {
        Self::from_token(
            Token::Dimension {
                has_sign: false,
                value: number as f32,
                int_value: Some(number),
                unit: CowRcStr::from("ms"),
            },
            url_data,
        )
    }

    /// Create VariableValue from an integer amount of CSS pixels.
    fn int_pixels(number: i32, url_data: &UrlExtraData) -> Self {
        Self::from_token(
            Token::Dimension {
                has_sign: false,
                value: number as f32,
                int_value: Some(number),
                unit: CowRcStr::from("px"),
            },
            url_data,
        )
    }

    fn from_token(token: Token, url_data: &UrlExtraData) -> Self {
        let token_type = token.serialization_type();
        let mut css = token.to_css_string();
        css.shrink_to_fit();

        VariableValue {
            css,
            url_data: url_data.clone(),
            first_token_type: token_type,
            last_token_type: token_type,
            references: Default::default(),
        }
    }

    /// Returns the raw CSS text from this VariableValue
    pub fn css_text(&self) -> &str {
        &self.css
    }

    /// Returns whether this variable value has any reference to the environment or other
    /// variables.
    pub fn has_references(&self) -> bool {
        self.references.has_references()
    }
}

/// <https://drafts.csswg.org/css-syntax-3/#typedef-declaration-value>
fn parse_declaration_value<'i, 't>(
    input: &mut Parser<'i, 't>,
    input_start: SourcePosition,
    namespaces: Option<&FxHashMap<Prefix, Namespace>>,
    references: &mut References,
    missing_closing_characters: &mut String,
) -> Result<(TokenSerializationType, TokenSerializationType), ParseError<'i>> {
    input.parse_until_before(Delimiter::Bang | Delimiter::Semicolon, |input| {
        parse_declaration_value_block(
            input,
            input_start,
            namespaces,
            references,
            missing_closing_characters,
        )
    })
}

/// Like parse_declaration_value, but accept `!` and `;` since they are only invalid at the top level.
fn parse_declaration_value_block<'i, 't>(
    input: &mut Parser<'i, 't>,
    input_start: SourcePosition,
    namespaces: Option<&FxHashMap<Prefix, Namespace>>,
    references: &mut References,
    missing_closing_characters: &mut String,
) -> Result<(TokenSerializationType, TokenSerializationType), ParseError<'i>> {
    let mut is_first = true;
    let mut first_token_type = TokenSerializationType::Nothing;
    let mut last_token_type = TokenSerializationType::Nothing;
    let mut prev_reference_index: Option<usize> = None;
    loop {
        let token_start = input.position();
        let Ok(token) = input.next_including_whitespace_and_comments() else {
            break;
        };

        let prev_token_type = last_token_type;
        let serialization_type = token.serialization_type();
        last_token_type = serialization_type;
        if is_first {
            first_token_type = last_token_type;
            is_first = false;
        }

        macro_rules! nested {
            ($closing:expr) => {{
                let mut inner_end_position = None;
                let result = input.parse_nested_block(|input| {
                    let result = parse_declaration_value_block(
                        input,
                        input_start,
                        namespaces,
                        references,
                        missing_closing_characters,
                    )?;
                    inner_end_position = Some(input.position());
                    Ok(result)
                })?;
                if inner_end_position.unwrap() == input.position() {
                    missing_closing_characters.push_str($closing);
                }
                result
            }};
        }
        if let Some(index) = prev_reference_index.take() {
            references.refs[index].next_token_type = serialization_type;
        }
        match *token {
            Token::Comment(_) => {
                let token_slice = input.slice_from(token_start);
                if !token_slice.ends_with("*/") {
                    missing_closing_characters.push_str(if token_slice.ends_with('*') {
                        "/"
                    } else {
                        "*/"
                    })
                }
            },
            Token::BadUrl(ref u) => {
                let e = StyleParseErrorKind::BadUrlInDeclarationValueBlock(u.clone());
                return Err(input.new_custom_error(e));
            },
            Token::BadString(ref s) => {
                let e = StyleParseErrorKind::BadStringInDeclarationValueBlock(s.clone());
                return Err(input.new_custom_error(e));
            },
            Token::CloseParenthesis => {
                let e = StyleParseErrorKind::UnbalancedCloseParenthesisInDeclarationValueBlock;
                return Err(input.new_custom_error(e));
            },
            Token::CloseSquareBracket => {
                let e = StyleParseErrorKind::UnbalancedCloseSquareBracketInDeclarationValueBlock;
                return Err(input.new_custom_error(e));
            },
            Token::CloseCurlyBracket => {
                let e = StyleParseErrorKind::UnbalancedCloseCurlyBracketInDeclarationValueBlock;
                return Err(input.new_custom_error(e));
            },
            Token::Function(ref name) => {
                let substitution_kind = match SubstitutionFunctionKind::from_ident(name).ok() {
                    Some(SubstitutionFunctionKind::Attr) => {
                        if static_prefs::pref!("layout.css.attr.enabled") {
                            Some(SubstitutionFunctionKind::Attr)
                        } else {
                            None
                        }
                    },
                    kind => kind,
                };
                if let Some(substitution_kind) = substitution_kind {
                    let our_ref_index = references.refs.len();
                    let mut input_end_position = None;
                    let fallback = input.parse_nested_block(|input| {
                        let mut namespace = ParsedNamespace::Known(Namespace::default());
                        if substitution_kind == SubstitutionFunctionKind::Attr {
                            if let Some(namespaces) = namespaces {
                                if let Ok(ns) = input
                                    .try_parse(|input| ParsedNamespace::parse(namespaces, input))
                                {
                                    namespace = ns;
                                    let prev = input.state();
                                    let next = match *input.next_including_whitespace()? {
                                        Token::Ident(_) => Ok(()),
                                        ref t => Err(prev
                                            .source_location()
                                            .new_unexpected_token_error(t.clone())),
                                    };
                                    input.reset(&prev);
                                    next?;
                                }
                            }
                        }
                        // TODO(emilio): For env() this should be <custom-ident> per spec, but no other browser does
                        // that, see https://github.com/w3c/csswg-drafts/issues/3262.
                        let name = input.expect_ident()?;
                        let name =
                            Atom::from(if substitution_kind == SubstitutionFunctionKind::Var {
                                match parse_name(name.as_ref()) {
                                    Ok(name) => name,
                                    Err(()) => {
                                        let name = name.clone();
                                        return Err(input.new_custom_error(
                                            SelectorParseErrorKind::UnexpectedIdent(name),
                                        ));
                                    },
                                }
                            } else {
                                name.as_ref()
                            });

                        let attribute_kind = if substitution_kind == SubstitutionFunctionKind::Attr
                        {
                            parse_attr_type(input)
                        } else {
                            AttributeType::None
                        };

                        // We want the order of the references to match source order. So we need to reserve our slot
                        // now, _before_ parsing our fallback. Note that we don't care if parsing fails after all, since
                        // if this fails we discard the whole result anyways.
                        let start = token_start.byte_index() - input_start.byte_index();
                        references.refs.push(SubstitutionFunctionReference {
                            name,
                            start,
                            // To be fixed up after parsing fallback and auto-closing via our_ref_index.
                            end: start,
                            prev_token_type,
                            // To be fixed up (if needed) on the next loop iteration via prev_reference_index.
                            next_token_type: TokenSerializationType::Nothing,
                            // To be fixed up after parsing fallback.
                            fallback: None,
                            attribute_data: AttributeData {
                                kind: attribute_kind,
                                namespace,
                            },
                            substitution_kind: substitution_kind.clone(),
                        });

                        let mut fallback = None;
                        if input.try_parse(|input| input.expect_comma()).is_ok() {
                            input.skip_whitespace();
                            let fallback_start = num::NonZeroUsize::new(
                                input.position().byte_index() - input_start.byte_index(),
                            )
                            .unwrap();
                            // NOTE(emilio): Intentionally using parse_declaration_value rather than
                            // parse_declaration_value_block, since that's what parse_fallback used to do.
                            let (first, last) = parse_declaration_value(
                                input,
                                input_start,
                                namespaces,
                                references,
                                missing_closing_characters,
                            )?;
                            fallback = Some(VariableFallback {
                                start: fallback_start,
                                first_token_type: first,
                                last_token_type: last,
                            });
                            input_end_position = Some(input.position());
                        } else {
                            let state = input.state();
                            // We still need to consume the rest of the potentially-unclosed
                            // tokens, but make sure to not consume tokens that would otherwise be
                            // invalid, by calling reset().
                            parse_declaration_value_block(
                                input,
                                input_start,
                                namespaces,
                                references,
                                missing_closing_characters,
                            )?;
                            input_end_position = Some(input.position());
                            input.reset(&state);
                        }
                        Ok(fallback)
                    })?;
                    if input_end_position.unwrap() == input.position() {
                        missing_closing_characters.push_str(")");
                    }
                    prev_reference_index = Some(our_ref_index);
                    let reference = &mut references.refs[our_ref_index];
                    reference.end = input.position().byte_index() - input_start.byte_index()
                        + missing_closing_characters.len();
                    reference.fallback = fallback;
                    match substitution_kind {
                        SubstitutionFunctionKind::Var => references.any_var = true,
                        SubstitutionFunctionKind::Env => references.any_env = true,
                        SubstitutionFunctionKind::Attr => references.any_attr = true,
                    };
                } else {
                    nested!(")");
                }
            },
            Token::ParenthesisBlock => {
                nested!(")");
            },
            Token::CurlyBracketBlock => {
                nested!("}");
            },
            Token::SquareBracketBlock => {
                nested!("]");
            },
            Token::QuotedString(_) => {
                let token_slice = input.slice_from(token_start);
                let quote = &token_slice[..1];
                debug_assert!(matches!(quote, "\"" | "'"));
                if !(token_slice.ends_with(quote) && token_slice.len() > 1) {
                    missing_closing_characters.push_str(quote)
                }
            },
            Token::Ident(ref value)
            | Token::AtKeyword(ref value)
            | Token::Hash(ref value)
            | Token::IDHash(ref value)
            | Token::UnquotedUrl(ref value)
            | Token::Dimension {
                unit: ref value, ..
            } => {
                references
                    .non_custom_references
                    .insert(NonCustomReferences::from_unit(value));
                let is_unquoted_url = matches!(token, Token::UnquotedUrl(_));
                if value.ends_with("�") && input.slice_from(token_start).ends_with("\\") {
                    // Unescaped backslash at EOF in these contexts is interpreted as U+FFFD
                    // Check the value in case the final backslash was itself escaped.
                    // Serialize as escaped U+FFFD, which is also interpreted as U+FFFD.
                    // (Unescaped U+FFFD would also work, but removing the backslash is annoying.)
                    missing_closing_characters.push_str("�")
                }
                if is_unquoted_url && !input.slice_from(token_start).ends_with(")") {
                    missing_closing_characters.push_str(")");
                }
            },
            _ => {},
        };
    }
    Ok((first_token_type, last_token_type))
}

/// Parse <attr-type> = type( <syntax> ) | raw-string | number | <attr-unit>.
/// https://drafts.csswg.org/css-values-5/#attr-notation
fn parse_attr_type<'i, 't>(input: &mut Parser<'i, 't>) -> AttributeType {
    input
        .try_parse(|input| {
            Ok(match input.next()? {
                Token::Function(ref name) if name.eq_ignore_ascii_case("type") => {
                    AttributeType::Type(
                        input.parse_nested_block(SyntaxDescriptor::from_css_parser)?,
                    )
                },
                Token::Ident(ref ident) => {
                    if ident.eq_ignore_ascii_case("raw-string") {
                        AttributeType::RawString
                    } else if let Ok(unit) = AttrUnit::from_ident(ident) {
                        AttributeType::Unit(unit)
                    } else {
                        AttributeType::Invalid
                    }
                },
                Token::Delim('%') => AttributeType::Unit(AttrUnit::Percentage),
                _ => return Err(input.new_custom_error(StyleParseErrorKind::UnspecifiedError)),
            })
        })
        .unwrap_or(AttributeType::None)
}

/// Attribute values may reference other substitution functions we may need to process.
/// See step 6: https://drafts.csswg.org/css-values-5/#attr-substitution
fn get_attr_value_for_cycle_resolution(
    name: &Atom,
    attribute_data: &AttributeData,
    url_data: &UrlExtraData,
    attribute_tracker: &mut AttributeTracker,
) -> Result<ComputedRegisteredValue, ()> {
    // If the attribute is not specified by the type() syntax, it
    // has no reference to traverse and we can stop processing.
    if !matches!(&attribute_data.kind, AttributeType::Type(_)) {
        return Err(());
    }
    #[cfg(feature = "gecko")]
    let local_name = LocalName::cast(name);
    #[cfg(feature = "servo")]
    let local_name = &LocalName::from(name.as_ref());
    let namespace = match attribute_data.namespace {
        ParsedNamespace::Known(ref ns) => ns,
        ParsedNamespace::Unknown => return Err(()),
    };
    let attr = attribute_tracker.query(local_name, namespace).ok_or(())?;
    let mut input = ParserInput::new(&attr);
    let mut parser = Parser::new(&mut input);
    // TODO(Bug 2021110): Support namespaced attributes in chained references.
    let value = VariableValue::parse(&mut parser, None, &url_data).map_err(|_| ())?;
    Ok(ComputedRegisteredValue::universal(Arc::new(value)))
}

#[derive(Default)]
struct SeenSubstitutionFunctions<'a> {
    var: PrecomputedHashSet<&'a Name>,
    attr: PrecomputedHashSet<&'a Name>,
}

/// A struct that takes care of encapsulating the cascade process for custom properties.
pub struct CustomPropertiesBuilder<'a, 'b: 'a> {
    seen: SeenSubstitutionFunctions<'a>,
    may_have_cycles: bool,
    has_color_scheme: bool,
    substitution_functions: ComputedSubstitutionFunctions,
    reverted: PrecomputedHashMap<&'a Name, (CascadePriority, RevertKind)>,
    stylist: &'a Stylist,
    computed_context: &'a mut computed::Context<'b>,
    references_from_non_custom_properties: NonCustomReferenceMap<Vec<Name>>,
}

fn find_non_custom_references(
    registration: &PropertyDescriptors,
    value: &VariableValue,
    may_have_color_scheme: bool,
    is_root_element: bool,
    include_universal: bool,
) -> Option<NonCustomReferences> {
    let syntax = registration.syntax.as_ref()?;
    let dependent_types = syntax.dependent_types();
    let may_reference_length = dependent_types.intersects(DependentDataTypes::LENGTH)
        || (include_universal && syntax.is_universal());
    if may_reference_length {
        let value_dependencies = value.references.non_custom_references(is_root_element);
        if !value_dependencies.is_empty() {
            return Some(value_dependencies);
        }
    }
    if dependent_types.intersects(DependentDataTypes::COLOR) && may_have_color_scheme {
        // NOTE(emilio): We might want to add a NonCustomReferences::COLOR_SCHEME or something but
        // it's not really needed for correctness, so for now we use an Option for that to signal
        // that there might be a dependency.
        return Some(NonCustomReferences::empty());
    }
    None
}

impl<'a, 'b: 'a> CustomPropertiesBuilder<'a, 'b> {
    /// Create a new builder, inheriting from a given custom properties map.
    ///
    /// We expose this publicly mostly for @keyframe blocks.
    pub fn new_with_properties(
        stylist: &'a Stylist,
        custom_properties: ComputedCustomProperties,
        computed_context: &'a mut computed::Context<'b>,
    ) -> Self {
        Self {
            seen: SeenSubstitutionFunctions::default(),
            reverted: Default::default(),
            may_have_cycles: false,
            has_color_scheme: false,
            substitution_functions: ComputedSubstitutionFunctions::new(
                Some(custom_properties),
                None,
            ),
            stylist,
            computed_context,
            references_from_non_custom_properties: NonCustomReferenceMap::default(),
        }
    }

    /// Create a new builder, inheriting from the right style given context.
    pub fn new(stylist: &'a Stylist, context: &'a mut computed::Context<'b>) -> Self {
        let is_root_element = context.is_root_element();

        let inherited = context.inherited_custom_properties();
        let initial_values = stylist.get_custom_property_initial_values();
        let properties = ComputedCustomProperties {
            inherited: if is_root_element {
                debug_assert!(inherited.is_empty());
                initial_values.inherited.clone()
            } else {
                inherited.inherited.clone()
            },
            non_inherited: initial_values.non_inherited.clone(),
        };

        // Reuse flags from computing registered custom properties initial values, such as
        // whether they depend on viewport units.
        context
            .style()
            .add_flags(stylist.get_custom_property_initial_values_flags());
        Self::new_with_properties(stylist, properties, context)
    }

    /// Cascade a given custom property declaration.
    pub fn cascade(
        &mut self,
        declaration: &'a CustomDeclaration,
        priority: CascadePriority,
        attribute_tracker: &mut AttributeTracker,
    ) {
        let CustomDeclaration {
            ref name,
            ref value,
        } = *declaration;

        if let Some(&(reverted_priority, revert_kind)) = self.reverted.get(&name) {
            if !reverted_priority.allows_when_reverted(&priority, revert_kind) {
                return;
            }
        }

        if !(priority.flags() - self.computed_context.included_cascade_flags).is_empty() {
            return;
        }

        let was_already_present = !self.seen.var.insert(name);
        if was_already_present {
            return;
        }

        if !self.value_may_affect_style(name, value) {
            return;
        }

        let kind = SubstitutionFunctionKind::Var;
        let map = &mut self.substitution_functions;
        let registration = self.stylist.get_custom_property_registration(&name);
        match value {
            CustomDeclarationValue::Unparsed(unparsed_value) => {
                // At this point of the cascade we're not guaranteed to have seen the color-scheme
                // declaration, so need to assume the worst. We could track all system color
                // keyword tokens + the light-dark() function, but that seems non-trivial /
                // probably overkill.
                let may_have_color_scheme = true;
                // Non-custom dependency is really relevant for registered custom properties
                // that require computed value of such dependencies.
                let has_dependency = unparsed_value.references.any_var
                    || unparsed_value.references.any_attr
                    || find_non_custom_references(
                        registration,
                        unparsed_value,
                        may_have_color_scheme,
                        self.computed_context.is_root_element(),
                        /* include_unregistered = */ false,
                    )
                    .is_some();
                // If the variable value has no references to other properties, perform
                // substitution here instead of forcing a full traversal in `substitute_all`
                // afterwards.
                if !has_dependency {
                    return substitute_references_if_needed_and_apply(
                        name,
                        kind,
                        unparsed_value,
                        &mut self.substitution_functions,
                        self.stylist,
                        self.computed_context,
                        attribute_tracker,
                    );
                }
                self.may_have_cycles = true;
                let value = ComputedRegisteredValue::universal(Arc::clone(unparsed_value));
                map.insert_var(registration, name, value);
            },
            CustomDeclarationValue::Parsed(parsed_value) => {
                let value = parsed_value.to_computed_value(&self.computed_context);
                map.insert_var(registration, name, value);
            },
            CustomDeclarationValue::CSSWideKeyword(keyword) => match keyword.revert_kind() {
                Some(revert_kind) => {
                    self.seen.var.remove(name);
                    self.reverted.insert(name, (priority, revert_kind));
                },
                None => match keyword {
                    CSSWideKeyword::Initial => {
                        // For non-inherited custom properties, 'initial' was handled in value_may_affect_style.
                        debug_assert!(registration.inherits(), "Should've been handled earlier");
                        remove_and_insert_initial_value(name, registration, map);
                    },
                    CSSWideKeyword::Inherit => {
                        // For inherited custom properties, 'inherit' was handled in value_may_affect_style.
                        debug_assert!(!registration.inherits(), "Should've been handled earlier");
                        self.computed_context
                            .style()
                            .add_flags(ComputedValueFlags::INHERITS_RESET_STYLE);
                        if let Some(inherited_value) = self
                            .computed_context
                            .inherited_custom_properties()
                            .non_inherited
                            .get(name)
                        {
                            map.insert_var(registration, name, inherited_value.clone());
                        }
                    },
                    // handled in value_may_affect_style or in the revert_kind branch above.
                    CSSWideKeyword::Revert
                    | CSSWideKeyword::RevertLayer
                    | CSSWideKeyword::RevertRule
                    | CSSWideKeyword::Unset => unreachable!(),
                },
            },
        }
    }

    /// Fast check to avoid calling maybe_note_non_custom_dependency in ~all cases.
    #[inline]
    pub fn might_have_non_custom_or_attr_dependency(
        id: LonghandId,
        decl: &PropertyDeclaration,
    ) -> bool {
        if id == LonghandId::ColorScheme {
            return true;
        }
        if let PropertyDeclaration::WithVariables(v) = decl {
            return matches!(id, LonghandId::LineHeight | LonghandId::FontSize)
                || v.value.variable_value.references.any_attr;
        }
        false
    }

    /// Note a non-custom property with variable reference that may in turn depend on that property.
    /// e.g. `font-size` depending on a custom property that may be a registered property using `em`.
    pub fn maybe_note_non_custom_dependency(
        &mut self,
        id: LonghandId,
        decl: &'a PropertyDeclaration,
        attribute_tracker: &mut AttributeTracker,
    ) {
        debug_assert!(Self::might_have_non_custom_or_attr_dependency(id, decl));
        if id == LonghandId::ColorScheme {
            // If we might change the color-scheme, we need to defer computation of colors.
            self.has_color_scheme = true;
            return;
        }

        let PropertyDeclaration::WithVariables(v) = decl else {
            return;
        };
        let value = &v.value.variable_value;
        let refs = &value.references;

        if !refs.any_var && !refs.any_attr {
            return;
        }

        // Attributes in non-custom properties may reference `var()` or `attr()` in their
        // values, which we need to track to support chained references and detect cycles.
        // Further processing occurs during `CustomPropertiesBuilder::build()`.
        if refs.any_attr {
            self.update_attributes_map(value, attribute_tracker);
            if !refs.any_var {
                return;
            }
        }

        // With unit algebra in `calc()`, references aren't limited to `font-size`.
        // For example, `--foo: 100ex; font-weight: calc(var(--foo) / 1ex);`,
        // or `--foo: 1em; zoom: calc(var(--foo) * 30px / 2em);`
        let references = match id {
            LonghandId::FontSize => {
                if self.computed_context.is_root_element() {
                    NonCustomReferences::ROOT_FONT_UNITS
                } else {
                    NonCustomReferences::FONT_UNITS
                }
            },
            LonghandId::LineHeight => {
                if self.computed_context.is_root_element() {
                    NonCustomReferences::ROOT_LH_UNITS | NonCustomReferences::ROOT_FONT_UNITS
                } else {
                    NonCustomReferences::LH_UNITS | NonCustomReferences::FONT_UNITS
                }
            },
            _ => return,
        };

        let variables: Vec<Atom> = refs
            .refs
            .iter()
            .filter_map(|reference| {
                if reference.substitution_kind != SubstitutionFunctionKind::Var {
                    return None;
                }
                let registration = self
                    .stylist
                    .get_custom_property_registration(&reference.name);
                if !registration
                    .syntax
                    .as_ref()?
                    .dependent_types()
                    .intersects(DependentDataTypes::LENGTH)
                {
                    return None;
                }
                Some(reference.name.clone())
            })
            .collect();
        references.for_each(|idx| {
            let entry = &mut self.references_from_non_custom_properties[idx];
            let was_none = entry.is_none();
            let v = entry.get_or_insert_with(|| variables.clone());
            if was_none {
                return;
            }
            v.extend(variables.iter().cloned());
        });
    }

    fn value_may_affect_style(&self, name: &Name, value: &CustomDeclarationValue) -> bool {
        let registration = self.stylist.get_custom_property_registration(&name);
        match *value {
            CustomDeclarationValue::CSSWideKeyword(CSSWideKeyword::Inherit) => {
                // For inherited custom properties, explicit 'inherit' means we
                // can just use any existing value in the inherited
                // CustomPropertiesMap.
                if registration.inherits() {
                    return false;
                }
            },
            CustomDeclarationValue::CSSWideKeyword(CSSWideKeyword::Initial) => {
                // For non-inherited custom properties, explicit 'initial' means
                // we can just use any initial value in the registration.
                if !registration.inherits() {
                    return false;
                }
            },
            CustomDeclarationValue::CSSWideKeyword(CSSWideKeyword::Unset) => {
                // Explicit 'unset' means we can either just use any existing
                // value in the inherited CustomPropertiesMap or the initial
                // value in the registration.
                return false;
            },
            _ => {},
        }

        let existing_value = self.substitution_functions.get_var(registration, &name);
        let Some(existing_value) = existing_value else {
            if matches!(
                value,
                CustomDeclarationValue::CSSWideKeyword(CSSWideKeyword::Initial)
            ) {
                debug_assert!(registration.inherits(), "Should've been handled earlier");
                // The initial value of a custom property without a
                // guaranteed-invalid initial value is the same as it
                // not existing in the map.
                if registration.initial_value.is_none() {
                    return false;
                }
            }
            return true;
        };
        match value {
            CustomDeclarationValue::Unparsed(value) => {
                // Don't bother overwriting an existing value with the same
                // specified value.
                if let Some(existing_value) = existing_value.as_universal() {
                    return existing_value != value;
                }
            },
            CustomDeclarationValue::Parsed(..) => {
                // If the value has dependencies, self.computed_context might not yield the same
                // result as the eventual value.
            },
            CustomDeclarationValue::CSSWideKeyword(kw) => {
                match kw {
                    CSSWideKeyword::Inherit => {
                        debug_assert!(!registration.inherits(), "Should've been handled earlier");
                        // existing_value is the registered initial value.
                        // Don't bother adding it to self.custom_properties.non_inherited
                        // if the key is also absent from self.inherited.non_inherited.
                        if self
                            .computed_context
                            .inherited_custom_properties()
                            .non_inherited
                            .get(name)
                            .is_none()
                        {
                            return false;
                        }
                    },
                    CSSWideKeyword::Initial => {
                        debug_assert!(registration.inherits(), "Should've been handled earlier");
                        // Don't bother overwriting an existing value with the initial value specified in
                        // the registration.
                        if let Some(initial_value) = self
                            .stylist
                            .get_custom_property_initial_values()
                            .get(registration, name)
                        {
                            return existing_value != initial_value;
                        }
                    },
                    CSSWideKeyword::Unset => {
                        debug_assert!(false, "Should've been handled earlier");
                    },
                    CSSWideKeyword::Revert
                    | CSSWideKeyword::RevertLayer
                    | CSSWideKeyword::RevertRule => {},
                }
            },
        };

        true
    }

    /// For a given unparsed variable, update the attributes map with its attr references.
    pub fn update_attributes_map(
        &mut self,
        value: &'a VariableValue,
        attribute_tracker: &mut AttributeTracker,
    ) {
        let refs = &value.references;
        if !refs.any_attr {
            return;
        }
        self.may_have_cycles = true;

        for next in &refs.refs {
            // Skip non-attrs and attributes we've already processed.
            if next.substitution_kind != SubstitutionFunctionKind::Attr
                || !self.seen.attr.insert(&next.name)
            {
                continue;
            }
            if let Ok(v) = get_attr_value_for_cycle_resolution(
                &next.name,
                &next.attribute_data,
                &value.url_data,
                attribute_tracker,
            ) {
                self.substitution_functions.insert_attr(&next.name, v);
            }
        }
    }

    /// Computes the map of applicable custom properties, as well as
    /// longhand properties that are now considered invalid-at-compute time.
    /// The result is saved into the computed context.
    ///
    /// If there was any specified property or non-inherited custom property
    /// with an initial value, we've created a new map and now we
    /// need to remove any potential cycles (And marking non-custom
    /// properties), and wrap it in an arc.
    ///
    /// Some registered custom properties may require font-related properties
    /// be resolved to resolve. If these properties are not resolved at this time,
    /// `defer` should be set to `Yes`, which will leave such custom properties,
    /// and other properties referencing them, untouched. These properties are
    /// returned separately, to be resolved by `build_deferred` to fully resolve
    /// all custom properties after all necessary non-custom properties are resolved.
    pub fn build(
        mut self,
        defer: DeferFontRelativeCustomPropertyResolution,
        attribute_tracker: &mut AttributeTracker,
    ) -> Option<AllSubstitutionFunctions> {
        let mut deferred_substitution_functions = None;
        if self.may_have_cycles {
            if defer == DeferFontRelativeCustomPropertyResolution::Yes {
                deferred_substitution_functions = Some(AllSubstitutionFunctions::default());
            }
            let mut invalid_non_custom_properties = LonghandIdSet::default();
            substitute_all(
                &mut self.substitution_functions,
                deferred_substitution_functions.as_mut(),
                &mut invalid_non_custom_properties,
                self.has_color_scheme,
                &self.seen,
                &self.references_from_non_custom_properties,
                self.stylist,
                self.computed_context,
                attribute_tracker,
            );
            self.computed_context.builder.invalid_non_custom_properties =
                invalid_non_custom_properties;
        }
        self.substitution_functions
            .custom_properties
            .shrink_to_fit();

        // Some pages apply a lot of redundant custom properties, see e.g.
        // bug 1758974 comment 5. Try to detect the case where the values
        // haven't really changed, and save some memory by reusing the inherited
        // map in that case.
        let initial_values = self.stylist.get_custom_property_initial_values();
        let custom_properties = self.substitution_functions.custom_properties;
        self.computed_context
            .builder
            .substitution_functions
            .custom_properties = ComputedCustomProperties {
            inherited: if self
                .computed_context
                .inherited_custom_properties()
                .inherited
                == custom_properties.inherited
            {
                self.computed_context
                    .inherited_custom_properties()
                    .inherited
                    .clone()
            } else {
                custom_properties.inherited
            },
            non_inherited: if initial_values.non_inherited == custom_properties.non_inherited {
                initial_values.non_inherited.clone()
            } else {
                custom_properties.non_inherited
            },
        };
        self.computed_context
            .builder
            .substitution_functions
            .attributes = self.substitution_functions.attributes;

        deferred_substitution_functions
    }

    /// Fully resolve all deferred custom properties and attributes, assuming that the
    /// incoming context has necessary properties resolved.
    pub fn build_deferred(
        deferred: AllSubstitutionFunctions,
        stylist: &Stylist,
        computed_context: &mut computed::Context,
        attribute_tracker: &mut AttributeTracker,
    ) {
        if deferred.is_empty() {
            return;
        }
        let mut map = std::mem::take(&mut computed_context.builder.substitution_functions);
        // Since `CustomPropertiesMap` preserves insertion order, we shouldn't have to worry about
        // resolving in a wrong order.
        for (name, kind, v) in deferred.iter() {
            let Some(v) = v.as_universal() else {
                unreachable!("Computing should have been deferred!")
            };
            substitute_references_if_needed_and_apply(
                name,
                kind,
                v,
                &mut map,
                stylist,
                computed_context,
                attribute_tracker,
            );
        }
        computed_context.builder.substitution_functions = map;
    }
}

/// Resolve all custom properties to either substituted, invalid, or unset
/// (meaning we should use the inherited value).
///
/// It does cycle dependencies removal at the same time as substitution.
fn substitute_all(
    substitution_function_map: &mut ComputedSubstitutionFunctions,
    mut deferred_substituted_functions_map: Option<&mut AllSubstitutionFunctions>,
    invalid_non_custom_properties: &mut LonghandIdSet,
    has_color_scheme: bool,
    seen: &SeenSubstitutionFunctions,
    references_from_non_custom_properties: &NonCustomReferenceMap<Vec<Name>>,
    stylist: &Stylist,
    computed_context: &computed::Context,
    attr_tracker: &mut AttributeTracker,
) {
    // The cycle dependencies removal in this function is a variant
    // of Tarjan's algorithm. It is mostly based on the pseudo-code
    // listed in
    // https://en.wikipedia.org/w/index.php?
    // title=Tarjan%27s_strongly_connected_components_algorithm&oldid=801728495

    #[derive(Clone, Eq, PartialEq, Debug)]
    enum VarType {
        Attr(Name),
        Custom(Name),
        NonCustom(SingleNonCustomReference),
    }

    /// Struct recording necessary information for each variable.
    #[derive(Debug)]
    struct VarInfo {
        /// The name of the variable. It will be taken to save addref
        /// when the corresponding variable is popped from the stack.
        /// This also serves as a mark for whether the variable is
        /// currently in the stack below.
        var: Option<VarType>,
        /// If the variable is in a dependency cycle, lowlink represents
        /// a smaller index which corresponds to a variable in the same
        /// strong connected component, which is known to be accessible
        /// from this variable. It is not necessarily the root, though.
        lowlink: usize,
    }

    #[derive(Debug, Default)]
    struct OrderIndexMap {
        /// The map from the custom property name to its order index.
        var: PrecomputedHashMap<Name, usize>,
        /// The map from the attribute name to its order index.
        attr: PrecomputedHashMap<Name, usize>,
    }

    /// Context struct for traversing the variable graph, so that we can
    /// avoid referencing all the fields multiple times.
    struct Context<'a, 'b: 'a> {
        /// Number of variables visited. This is used as the order index
        /// when we visit a new unresolved variable.
        count: usize,
        /// The map from a substitution function name to its order index.
        index_map: OrderIndexMap,
        /// Mapping from a non-custom dependency to its order index.
        non_custom_index_map: NonCustomReferenceMap<usize>,
        /// Information of each variable indexed by the order index.
        var_info: SmallVec<[VarInfo; 5]>,
        /// The stack of order index of visited variables. It contains
        /// all unfinished strong connected components.
        stack: SmallVec<[usize; 5]>,
        /// References to non-custom properties in this strongly connected component.
        non_custom_references: NonCustomReferences,
        /// Whether the builder has seen a non-custom color-scheme reference.
        has_color_scheme: bool,
        /// Whether this strongly connected component contains any custom properties involving
        /// value computation.
        contains_computed_custom_property: bool,
        map: &'a mut ComputedSubstitutionFunctions,
        /// The stylist is used to get registered properties, and to resolve the environment to
        /// substitute `env()` variables.
        stylist: &'a Stylist,
        /// The computed context is used to get inherited custom
        /// properties  and compute registered custom properties.
        computed_context: &'a computed::Context<'b>,
        /// Longhand IDs that became invalid due to dependency cycle(s).
        invalid_non_custom_properties: &'a mut LonghandIdSet,
        /// Substitution functions that cannot yet be substituted. We store both custom
        /// properties (inherited and non-inherited) and attributes in the same map, since
        /// we need to make sure we iterate through them in the right order.
        deferred_substitution_functions: Option<&'a mut AllSubstitutionFunctions>,
    }

    /// This function combines the traversal for cycle removal and value
    /// substitution. It returns either a signal None if this variable
    /// has been fully resolved (to either having no reference or being
    /// marked invalid), or the order index for the given name.
    ///
    /// When it returns, the variable corresponds to the name would be
    /// in one of the following states:
    /// * It is still in context.stack, which means it is part of an
    ///   potentially incomplete dependency circle.
    /// * It has been removed from the map.  It can be either that the
    ///   substitution failed, or it is inside a dependency circle.
    ///   When this function removes a variable from the map because
    ///   of dependency circle, it would put all variables in the same
    ///   strong connected component to the set together.
    /// * It doesn't have any reference, because either this variable
    ///   doesn't have reference at all in specified value, or it has
    ///   been completely resolved.
    /// * There is no such variable at all.
    fn traverse<'a, 'b>(
        var: VarType,
        non_custom_references: &NonCustomReferenceMap<Vec<Name>>,
        context: &mut Context<'a, 'b>,
        attribute_tracker: &mut AttributeTracker,
    ) -> Option<usize> {
        let kind = if matches!(var, VarType::Custom(_)) {
            SubstitutionFunctionKind::Var
        } else {
            SubstitutionFunctionKind::Attr
        };
        // Some shortcut checks.
        let value = match var {
            VarType::Custom(ref name) | VarType::Attr(ref name) => {
                let registration;
                let value;
                match kind {
                    SubstitutionFunctionKind::Var => {
                        registration = context.stylist.get_custom_property_registration(name);
                        value = context.map.get_var(registration, name)?.as_universal()?;
                    },
                    SubstitutionFunctionKind::Attr => {
                        // `attr()` is always treated as unregistered.
                        registration = PropertyDescriptors::unregistered();
                        value = context.map.get_attr(name)?.as_universal()?;
                    },
                    _ => unreachable!("Substitution kind must be var or attr for VarType::Custom."),
                }
                let is_var = matches!(kind, SubstitutionFunctionKind::Var);
                let is_attr = matches!(kind, SubstitutionFunctionKind::Attr);
                let is_root = context.computed_context.is_root_element();
                // We need to keep track of potential non-custom-references even on unregistered
                // properties for cycle-detection purposes.
                let non_custom_refs = find_non_custom_references(
                    registration,
                    value,
                    context.has_color_scheme,
                    is_root,
                    /* include_unregistered = */ true,
                );
                context.non_custom_references |= non_custom_refs.unwrap_or_default();
                let has_dependency = value.references.any_var
                    || value.references.any_attr
                    || non_custom_refs.is_some();
                // Nothing to resolve.
                if !has_dependency {
                    debug_assert!(!value.references.any_env, "Should've been handled earlier");
                    if is_attr || !registration.is_universal() {
                        // We might still need to compute the value if this is not an universal
                        // registration if we thought this had a dependency before but turned out
                        // not to be (due to has_color_scheme, for example). Note that if this was
                        // already computed we would've bailed out in the as_universal() check.
                        if is_var {
                            debug_assert!(
                                registration
                                    .syntax
                                    .as_ref()
                                    .unwrap()
                                    .dependent_types()
                                    .intersects(DependentDataTypes::COLOR),
                                "How did an unresolved value get here otherwise?",
                            );
                        }
                        let value = value.clone();
                        substitute_references_if_needed_and_apply(
                            name,
                            kind,
                            &value,
                            &mut context.map,
                            context.stylist,
                            context.computed_context,
                            attribute_tracker,
                        );
                    }
                    return None;
                }

                // Has this variable been visited?
                let index_map = if is_var {
                    &mut context.index_map.var
                } else {
                    &mut context.index_map.attr
                };
                match index_map.entry(name.clone()) {
                    Entry::Occupied(entry) => {
                        return Some(*entry.get());
                    },
                    Entry::Vacant(entry) => {
                        entry.insert(context.count);
                    },
                }
                context.contains_computed_custom_property |= is_var && !registration.is_universal();

                // Hold a strong reference to the value so that we don't
                // need to keep reference to context.map.
                Some(value.clone())
            },
            VarType::NonCustom(ref non_custom) => {
                let entry = &mut context.non_custom_index_map[*non_custom];
                if let Some(v) = entry {
                    return Some(*v);
                }
                *entry = Some(context.count);
                None
            },
        };

        // Add new entry to the information table.
        let index = context.count;
        context.count += 1;
        debug_assert_eq!(index, context.var_info.len());
        context.var_info.push(VarInfo {
            var: Some(var.clone()),
            lowlink: index,
        });
        context.stack.push(index);

        let mut self_ref = false;
        let mut lowlink = index;
        let visit_link = |var: VarType,
                          context: &mut Context,
                          lowlink: &mut usize,
                          self_ref: &mut bool,
                          attr_tracker: &mut AttributeTracker| {
            let next_index = match traverse(var, non_custom_references, context, attr_tracker) {
                Some(index) => index,
                // There is nothing to do if the next variable has been
                // fully resolved at this point.
                None => {
                    return;
                },
            };
            let next_info = &context.var_info[next_index];
            if next_index > index {
                // The next variable has a larger index than us, so it
                // must be inserted in the recursive call above. We want
                // to get its lowlink.
                *lowlink = cmp::min(*lowlink, next_info.lowlink);
            } else if next_index == index {
                *self_ref = true;
            } else if next_info.var.is_some() {
                // The next variable has a smaller order index and it is
                // in the stack, so we are at the same component.
                *lowlink = cmp::min(*lowlink, next_index);
            }
        };
        if let Some(ref v) = value.as_ref() {
            debug_assert!(
                matches!(var, VarType::Custom(_) | VarType::Attr(_)),
                "Non-custom property has references?"
            );

            // Visit other custom properties...
            // FIXME: Maybe avoid visiting the same var twice if not needed?
            for next in &v.references.refs {
                if next.substitution_kind == SubstitutionFunctionKind::Env {
                    continue;
                }

                let next_var = if next.substitution_kind == SubstitutionFunctionKind::Attr {
                    if context.map.get_attr(&next.name).is_none() {
                        let Ok(val) = get_attr_value_for_cycle_resolution(
                            &next.name,
                            &next.attribute_data,
                            &v.url_data,
                            attribute_tracker,
                        ) else {
                            continue;
                        };
                        context.map.insert_attr(&next.name, val);
                    }
                    VarType::Attr(next.name.clone())
                } else {
                    VarType::Custom(next.name.clone())
                };

                visit_link(
                    next_var,
                    context,
                    &mut lowlink,
                    &mut self_ref,
                    attribute_tracker,
                );
            }

            // ... Then non-custom properties.
            v.references.non_custom_references.for_each(|r| {
                visit_link(
                    VarType::NonCustom(r),
                    context,
                    &mut lowlink,
                    &mut self_ref,
                    attribute_tracker,
                );
            });
        } else if let VarType::NonCustom(non_custom) = var {
            let entry = &non_custom_references[non_custom];
            if let Some(deps) = entry.as_ref() {
                for d in deps {
                    // Visit any reference from this non-custom property to custom properties.
                    // TODO(bug2026785): non-custom should traverse unregistered vars and attrs.
                    visit_link(
                        VarType::Custom(d.clone()),
                        context,
                        &mut lowlink,
                        &mut self_ref,
                        attribute_tracker,
                    );
                }
            }
        }

        context.var_info[index].lowlink = lowlink;
        if lowlink != index {
            // This variable is in a loop, but it is not the root of
            // this strong connected component. We simply return for
            // now, and the root would remove it from the map.
            //
            // This cannot be removed from the map here, because
            // otherwise the shortcut check at the beginning of this
            // function would return the wrong value.
            return Some(index);
        }

        // This is the root of a strong-connected component.
        let mut in_loop = self_ref;
        let name;

        let handle_variable_in_loop =
            |name: &Name, context: &mut Context<'a, 'b>, kind: SubstitutionFunctionKind| {
                if context.contains_computed_custom_property {
                    // These non-custom properties can't become invalid-at-compute-time from
                    // cyclic dependencies purely consisting of non-registered properties.
                    if context.non_custom_references.intersects(
                        NonCustomReferences::FONT_UNITS | NonCustomReferences::ROOT_FONT_UNITS,
                    ) {
                        context
                            .invalid_non_custom_properties
                            .insert(LonghandId::FontSize);
                    }
                    if context.non_custom_references.intersects(
                        NonCustomReferences::LH_UNITS | NonCustomReferences::ROOT_LH_UNITS,
                    ) {
                        context
                            .invalid_non_custom_properties
                            .insert(LonghandId::LineHeight);
                    }
                }
                // This variable is in loop. Resolve to invalid.
                handle_invalid_at_computed_value_time(
                    name,
                    kind,
                    &mut context.map,
                    context.computed_context,
                );
            };
        loop {
            let var_index = context
                .stack
                .pop()
                .expect("The current variable should still be in stack");
            let var_info = &mut context.var_info[var_index];
            // We should never visit the variable again, so it's safe
            // to take the name away, so that we don't do additional
            // reference count.
            let var_name = var_info
                .var
                .take()
                .expect("Variable should not be poped from stack twice");
            if var_index == index {
                name = match var_name {
                    VarType::Custom(name) | VarType::Attr(name) => name,
                    // At the root of this component, and it's a non-custom
                    // reference - we have nothing to substitute, so
                    // it's effectively resolved.
                    VarType::NonCustom(..) => return None,
                };
                break;
            }
            if let VarType::Custom(name) | VarType::Attr(name) = var_name {
                // Anything here is in a loop which can traverse to the
                // variable we are handling, so it's invalid at
                // computed-value time.
                handle_variable_in_loop(&name, context, kind);
            }
            in_loop = true;
        }
        // We've gotten to the root of this strongly connected component, so clear
        // whether or not it involved non-custom references.
        // It's fine to track it like this, because non-custom properties currently
        // being tracked can only participate in any loop only once.
        if in_loop {
            handle_variable_in_loop(&name, context, kind);
            context.non_custom_references = NonCustomReferences::default();
            return None;
        }

        if let Some(ref v) = value {
            let registration = if kind == SubstitutionFunctionKind::Var {
                context.stylist.get_custom_property_registration(&name)
            } else {
                PropertyDescriptors::unregistered()
            };

            let mut defer = false;
            if let Some(ref mut deferred) = context.deferred_substitution_functions {
                // We need to defer this property if it has a non-custom property dependency, or
                // any variable that it references is already deferred.
                defer = find_non_custom_references(
                    registration,
                    v,
                    context.has_color_scheme,
                    context.computed_context.is_root_element(),
                    /* include_unregistered = */ false,
                )
                .is_some()
                    || v.references.refs.iter().any(|reference| {
                        (reference.substitution_kind == SubstitutionFunctionKind::Var
                            && deferred
                                .get(&reference.name, SubstitutionFunctionKind::Var)
                                .is_some())
                            || reference.substitution_kind == SubstitutionFunctionKind::Attr
                    });
                if defer {
                    let value = ComputedRegisteredValue::universal(Arc::clone(v));
                    deferred.insert(&name, kind, value);
                    if kind == SubstitutionFunctionKind::Var {
                        context.map.remove_var(registration, &name);
                    } else {
                        context.map.remove_attr(&name);
                    }
                }
            }

            // If there are no var or attr references we should already be computed and substituted by now.
            if !defer && (v.references.any_var || v.references.any_attr) {
                substitute_references_if_needed_and_apply(
                    &name,
                    kind,
                    v,
                    &mut context.map,
                    context.stylist,
                    context.computed_context,
                    attribute_tracker,
                );
            }
        }
        context.non_custom_references = NonCustomReferences::default();

        // All resolved, so return the signal value.
        None
    }

    let mut run = |make_var: fn(Name) -> VarType, seen: &PrecomputedHashSet<&Name>| {
        for name in seen {
            let mut context = Context {
                count: 0,
                index_map: OrderIndexMap::default(),
                non_custom_index_map: NonCustomReferenceMap::default(),
                stack: SmallVec::new(),
                var_info: SmallVec::new(),
                map: substitution_function_map,
                non_custom_references: NonCustomReferences::default(),
                has_color_scheme,
                stylist,
                computed_context,
                invalid_non_custom_properties,
                deferred_substitution_functions: deferred_substituted_functions_map.as_deref_mut(),
                contains_computed_custom_property: false,
            };

            traverse(
                make_var((*name).clone()),
                references_from_non_custom_properties,
                &mut context,
                attr_tracker,
            );
        }
    };

    // Note that `seen` doesn't contain names inherited from our parent, but
    // those can't have variable references (since we inherit the computed
    // variables) so we don't want to spend cycles traversing them anyway.
    run(VarType::Custom, &seen.var);
    // Traverse potentially untraversed chained references from `attr(type())`
    // in non-custom properties.
    run(VarType::Attr, &seen.attr);
}

// See https://drafts.csswg.org/css-variables-2/#invalid-at-computed-value-time
fn handle_invalid_at_computed_value_time(
    name: &Name,
    kind: SubstitutionFunctionKind,
    substitution_functions: &mut ComputedSubstitutionFunctions,
    computed_context: &computed::Context,
) {
    if kind == SubstitutionFunctionKind::Attr {
        // Early return: `attr()` is always treated as unregistered.
        substitution_functions.remove_attr(name);
        return;
    }

    let stylist = computed_context.style().stylist.unwrap();
    let registration = stylist.get_custom_property_registration(&name);
    if !registration.is_universal() {
        // For the root element, inherited maps are empty. We should just
        // use the initial value if any, rather than removing the name.
        if registration.inherits() && !computed_context.is_root_element() {
            let inherited = computed_context.inherited_custom_properties();
            if let Some(value) = inherited.get(registration, name) {
                substitution_functions.insert_var(registration, name, value.clone());
                return;
            }
        } else if let Some(ref initial_value) = registration.initial_value {
            if let Ok(initial_value) = compute_value(
                &initial_value.css,
                &initial_value.url_data,
                registration,
                computed_context,
                AttrTaint::default(),
            ) {
                substitution_functions.insert_var(registration, name, initial_value);
                return;
            }
        }
    }
    substitution_functions.remove_var(registration, name);
}

/// Replace `var()`, `env()`, and `attr()` functions in a pre-existing variable value.
fn substitute_references_if_needed_and_apply(
    name: &Name,
    kind: SubstitutionFunctionKind,
    value: &Arc<VariableValue>,
    substitution_functions: &mut ComputedSubstitutionFunctions,
    stylist: &Stylist,
    computed_context: &computed::Context,
    attribute_tracker: &mut AttributeTracker,
) {
    debug_assert_ne!(kind, SubstitutionFunctionKind::Env);
    let is_var = matches!(kind, SubstitutionFunctionKind::Var);
    let registration = stylist.get_custom_property_registration(&name);
    if is_var && !value.has_references() && registration.is_universal() {
        // Trivial path: no references and no need to compute the value, just apply it directly.
        let computed_value = ComputedRegisteredValue::universal(Arc::clone(value));
        substitution_functions.insert_var(registration, name, computed_value);
        return;
    }

    let inherited = computed_context.inherited_custom_properties();
    let url_data = &value.url_data;
    let substitution = match substitute_internal(
        value,
        substitution_functions,
        stylist,
        computed_context,
        attribute_tracker,
        None,
    ) {
        Ok(v) => v,
        Err(..) => {
            handle_invalid_at_computed_value_time(
                name,
                kind,
                substitution_functions,
                computed_context,
            );
            return;
        },
    };

    // If variable fallback results in a wide keyword, deal with it now.
    if is_var {
        let css = &substitution.css;
        let css_wide_kw = {
            let mut input = ParserInput::new(&css);
            let mut input = Parser::new(&mut input);
            input.try_parse(CSSWideKeyword::parse)
        };

        if let Ok(kw) = css_wide_kw {
            // TODO: It's unclear what this should do for revert / revert-layer, see
            // https://github.com/w3c/csswg-drafts/issues/9131. For now treating as unset
            // seems fine?
            match (
                kw,
                registration.inherits(),
                computed_context.is_root_element(),
            ) {
                (CSSWideKeyword::Initial, _, _)
                | (CSSWideKeyword::Revert, false, _)
                | (CSSWideKeyword::RevertLayer, false, _)
                | (CSSWideKeyword::RevertRule, false, _)
                | (CSSWideKeyword::Unset, false, _)
                | (CSSWideKeyword::Revert, true, true)
                | (CSSWideKeyword::RevertLayer, true, true)
                | (CSSWideKeyword::RevertRule, true, true)
                | (CSSWideKeyword::Unset, true, true)
                | (CSSWideKeyword::Inherit, _, true) => {
                    remove_and_insert_initial_value(name, registration, substitution_functions);
                },
                (CSSWideKeyword::Revert, true, false)
                | (CSSWideKeyword::RevertLayer, true, false)
                | (CSSWideKeyword::RevertRule, true, false)
                | (CSSWideKeyword::Inherit, _, false)
                | (CSSWideKeyword::Unset, true, false) => {
                    match inherited.get(registration, name) {
                        Some(value) => {
                            substitution_functions.insert_var(registration, name, value.clone());
                        },
                        None => {
                            substitution_functions.remove_var(registration, name);
                        },
                    };
                },
            }
            return;
        }
    }

    match kind {
        SubstitutionFunctionKind::Var => {
            let value = match substitution.into_value(url_data, registration, computed_context) {
                Ok(v) => v,
                Err(()) => {
                    handle_invalid_at_computed_value_time(
                        name,
                        kind,
                        substitution_functions,
                        computed_context,
                    );
                    return;
                },
            };
            substitution_functions.insert_var(registration, name, value);
        },
        SubstitutionFunctionKind::Attr => {
            let mut value = ComputedRegisteredValue::universal(Arc::new(VariableValue::new(
                substitution.css.into_owned(),
                url_data,
                substitution.first_token_type,
                substitution.last_token_type,
            )));
            value.attr_tainted |= substitution.attr_tainted;
            substitution_functions.insert_attr(name, value);
        },
        SubstitutionFunctionKind::Env => unreachable!("Kind cannot be env."),
    }
}

#[derive(Default, Debug)]
struct Substitution<'a> {
    css: Cow<'a, str>,
    first_token_type: TokenSerializationType,
    last_token_type: TokenSerializationType,
    attr_tainted: bool,
}

impl<'a> Substitution<'a> {
    fn from_value(v: VariableValue, attr_tainted: bool) -> Self {
        Substitution {
            css: v.css.into(),
            first_token_type: v.first_token_type,
            last_token_type: v.last_token_type,
            attr_tainted,
        }
    }

    fn into_value(
        self,
        url_data: &UrlExtraData,
        registration: &PropertyDescriptors,
        computed_context: &computed::Context,
    ) -> Result<ComputedRegisteredValue, ()> {
        if registration.is_universal() {
            let mut value = ComputedRegisteredValue::universal(Arc::new(VariableValue::new(
                self.css.into_owned(),
                url_data,
                self.first_token_type,
                self.last_token_type,
            )));
            value.attr_tainted |= self.attr_tainted;
            return Ok(value);
        }
        let taint = if self.attr_tainted {
            // Per spec: substitution value of an arbitrary substitution function is
            // attr()-tainted as a whole if any attr()-tainted values were involved
            // in creating that substitution value.
            // https://drafts.csswg.org/css-values-5/#attr-security
            AttrTaint::new_fully_tainted(self.css.len())
        } else {
            AttrTaint::default()
        };
        let mut v = compute_value(&self.css, url_data, registration, computed_context, taint)?;
        v.attr_tainted |= self.attr_tainted;
        Ok(v)
    }

    fn new(
        css: Cow<'a, str>,
        first_token_type: TokenSerializationType,
        last_token_type: TokenSerializationType,
        attr_tainted: bool,
    ) -> Self {
        Self {
            css,
            first_token_type,
            last_token_type,
            attr_tainted,
        }
    }
}

/// Result of var(), env(), and attr() substitution.
#[derive(Debug)]
pub struct SubstitutionResult<'a> {
    /// The resolved CSS string after substitution.
    pub css: Cow<'a, str>,
    /// Regions in the `css` string that are attr()-tainted, if any.
    pub attr_taint: AttrTaint,
}

fn compute_value(
    css: &str,
    url_data: &UrlExtraData,
    registration: &PropertyDescriptors,
    computed_context: &computed::Context,
    attr_taint: AttrTaint,
) -> Result<ComputedRegisteredValue, ()> {
    debug_assert!(!registration.is_universal());

    let mut input = ParserInput::new(&css);
    let mut input = Parser::new(&mut input);

    SpecifiedRegisteredValue::compute(
        &mut input,
        registration,
        None,
        url_data,
        computed_context,
        AllowComputationallyDependent::Yes,
        attr_taint,
    )
}

/// Removes the named registered custom property and inserts its uncomputed initial value.
fn remove_and_insert_initial_value(
    name: &Name,
    registration: &PropertyDescriptors,
    substitution_functions: &mut ComputedSubstitutionFunctions,
) {
    substitution_functions.remove_var(registration, name);
    if let Some(ref initial_value) = registration.initial_value {
        let value = ComputedRegisteredValue::universal(Arc::clone(initial_value));
        substitution_functions.insert_var(registration, name, value);
    }
}

fn do_substitute_chunk<'a>(
    css: &'a str,
    start: usize,
    end: usize,
    first_token_type: TokenSerializationType,
    last_token_type: TokenSerializationType,
    url_data: &UrlExtraData,
    substitution_functions: &'a ComputedSubstitutionFunctions,
    stylist: &Stylist,
    computed_context: &computed::Context,
    references: &mut std::iter::Peekable<std::slice::Iter<SubstitutionFunctionReference>>,
    attribute_tracker: &mut AttributeTracker,
    mut attr_taint: Option<&mut AttrTaint>,
) -> Result<Substitution<'a>, ()> {
    if start == end {
        // Empty string. Easy.
        return Ok(Substitution::default());
    }
    // Easy case: no references involved.
    if references
        .peek()
        .map_or(true, |reference| reference.end > end)
    {
        let result = &css[start..end];
        return Ok(Substitution::new(
            Cow::Borrowed(result),
            first_token_type,
            last_token_type,
            Default::default(),
        ));
    }

    let mut substituted = ComputedValue::empty(url_data);
    let mut next_token_type = first_token_type;
    let mut cur_pos = start;
    let mut attr_tainted = false;
    while let Some(reference) = references.next_if(|reference| reference.end <= end) {
        if reference.start != cur_pos {
            substituted.push(
                &css[cur_pos..reference.start],
                next_token_type,
                reference.prev_token_type,
                /* attr_taint */ None,
            )?;
        }

        let substitution = substitute_one_reference(
            css,
            url_data,
            substitution_functions,
            reference,
            stylist,
            computed_context,
            references,
            attribute_tracker,
        )?;

        // Optimize the property: var(--...) case to avoid allocating at all.
        if reference.start == start && reference.end == end {
            if let Some(taint) = attr_taint.filter(|_| substitution.attr_tainted) {
                taint.push(start, end);
            }
            return Ok(substitution);
        }

        substituted.push(
            &substitution.css,
            substitution.first_token_type,
            substitution.last_token_type,
            attr_taint
                .as_deref_mut()
                .filter(|_| substitution.attr_tainted),
        )?;
        attr_tainted |= substitution.attr_tainted;
        next_token_type = reference.next_token_type;
        cur_pos = reference.end;
    }
    // Push the rest of the value if needed.
    if cur_pos != end {
        substituted.push(
            &css[cur_pos..end],
            next_token_type,
            last_token_type,
            /* attr_taint */ None,
        )?;
    }
    Ok(Substitution::from_value(substituted, attr_tainted))
}

fn quoted_css_string(src: &str) -> String {
    let mut dest = String::with_capacity(src.len() + 2);
    cssparser::serialize_string(src, &mut dest).unwrap();
    dest
}

fn substitute_one_reference<'a>(
    css: &'a str,
    url_data: &UrlExtraData,
    substitution_functions: &'a ComputedSubstitutionFunctions,
    reference: &SubstitutionFunctionReference,
    stylist: &Stylist,
    computed_context: &computed::Context,
    references: &mut std::iter::Peekable<std::slice::Iter<SubstitutionFunctionReference>>,
    attribute_tracker: &mut AttributeTracker,
) -> Result<Substitution<'a>, ()> {
    let simple_attr_subst = |s: &str| {
        Some(Substitution::new(
            Cow::Owned(quoted_css_string(s)),
            TokenSerializationType::Nothing,
            TokenSerializationType::Nothing,
            /* attr_tainted */ true,
        ))
    };
    let substitution: Option<_> = match reference.substitution_kind {
        SubstitutionFunctionKind::Var => {
            let registration = stylist.get_custom_property_registration(&reference.name);
            substitution_functions
                .get_var(registration, &reference.name)
                .map(|v| Substitution::from_value(v.to_variable_value(), v.attr_tainted))
        },
        SubstitutionFunctionKind::Env => {
            let device = stylist.device();
            device
                .environment()
                .get(&reference.name, device, url_data)
                .map(|v| Substitution::from_value(v, /* attr_tainted */ false))
        },
        // https://drafts.csswg.org/css-values-5/#attr-substitution
        SubstitutionFunctionKind::Attr => {
            #[cfg(feature = "gecko")]
            let local_name = LocalName::cast(&reference.name);
            #[cfg(feature = "servo")]
            let local_name = LocalName::from(reference.name.as_ref());
            let namespace = match reference.attribute_data.namespace {
                ParsedNamespace::Known(ref ns) => Some(ns),
                ParsedNamespace::Unknown => None,
            };
            namespace
                .and_then(|namespace| attribute_tracker.query(&local_name, namespace))
                .map_or_else(
                    || {
                        // Special case when fallback and <attr-type> are omitted.
                        // See FAILURE: https://drafts.csswg.org/css-values-5/#attr-substitution
                        if reference.fallback.is_none()
                            && reference.attribute_data.kind == AttributeType::None
                        {
                            simple_attr_subst("")
                        } else {
                            None
                        }
                    },
                    |attr| {
                        let attr = if let AttributeType::Type(_) = &reference.attribute_data.kind {
                            // If we're evaluating a container query, we haven't run the cascade
                            // and populated substitution_functions.attributes, so we can't do the
                            // get_attr() lookup here.
                            // TODO: This means chained attr() references will not work reliably in
                            // container style queries:
                            // https://bugzilla.mozilla.org/show_bug.cgi?id=2028861
                            if computed_context.in_container_query {
                                attr
                            } else {
                                substitution_functions
                                    .get_attr(&reference.name)
                                    .map(|v| v.to_variable_value())?
                                    .css
                            }
                        } else {
                            attr
                        };
                        let mut input = ParserInput::new(&attr);
                        let mut parser = Parser::new(&mut input);
                        match &reference.attribute_data.kind {
                            AttributeType::Unit(unit) => {
                                let css = {
                                    // Verify that attribute data is a <number-token>.
                                    parser.expect_number().ok()?;
                                    let mut s = attr.clone();
                                    s.push_str(unit.as_ref());
                                    s
                                };
                                let serialization = match unit {
                                    AttrUnit::Number => TokenSerializationType::Number,
                                    AttrUnit::Percentage => TokenSerializationType::Percentage,
                                    _ => TokenSerializationType::Dimension,
                                };
                                let value =
                                    ComputedValue::new(css, url_data, serialization, serialization);
                                Some(Substitution::from_value(
                                    value, /* attr_tainted */ true,
                                ))
                            },
                            AttributeType::Type(syntax) => {
                                let value = SpecifiedRegisteredValue::parse(
                                    &mut parser,
                                    &syntax,
                                    url_data,
                                    None,
                                    AllowComputationallyDependent::Yes,
                                    AttrTaint::default(),
                                )
                                .ok()?;
                                let value = value.to_variable_value();
                                Some(Substitution::from_value(
                                    value, /* attr_tainted */ true,
                                ))
                            },
                            AttributeType::RawString | AttributeType::None => {
                                simple_attr_subst(&attr)
                            },
                            AttributeType::Invalid => None,
                        }
                    },
                )
        },
    };

    if let Some(s) = substitution {
        // Skip references that are inside the outer variable (in fallback for example).
        while references
            .next_if(|next_ref| next_ref.end <= reference.end)
            .is_some()
        {}
        return Ok(s);
    }

    let Some(ref fallback) = reference.fallback else {
        return Err(());
    };

    do_substitute_chunk(
        css,
        fallback.start.get(),
        reference.end - 1, // Skip the closing parenthesis of the reference value.
        fallback.first_token_type,
        fallback.last_token_type,
        url_data,
        substitution_functions,
        stylist,
        computed_context,
        references,
        attribute_tracker,
        /* attr_taint */ None,
    )
}

/// Replace `var()`, `env()`, and `attr()` functions. Return `Err(..)` for invalid at computed time.
fn substitute_internal<'a>(
    variable_value: &'a VariableValue,
    substitution_functions: &'a ComputedSubstitutionFunctions,
    stylist: &Stylist,
    computed_context: &computed::Context,
    attribute_tracker: &mut AttributeTracker,
    mut attr_taint: Option<&mut AttrTaint>,
) -> Result<Substitution<'a>, ()> {
    let mut refs = variable_value.references.refs.iter().peekable();
    do_substitute_chunk(
        &variable_value.css,
        /* start = */ 0,
        /* end = */ variable_value.css.len(),
        variable_value.first_token_type,
        variable_value.last_token_type,
        &variable_value.url_data,
        substitution_functions,
        stylist,
        computed_context,
        &mut refs,
        attribute_tracker,
        attr_taint.as_deref_mut(),
    )
}

/// Replace var(), env(), and attr() functions, returning the resulting CSS string.
pub fn substitute<'a>(
    variable_value: &'a VariableValue,
    substitution_functions: &'a ComputedSubstitutionFunctions,
    stylist: &Stylist,
    computed_context: &computed::Context,
    attribute_tracker: &mut AttributeTracker,
) -> Result<SubstitutionResult<'a>, ()> {
    debug_assert!(variable_value.has_references());
    let mut attr_taint = AttrTaint::default();
    let v = substitute_internal(
        variable_value,
        substitution_functions,
        stylist,
        computed_context,
        attribute_tracker,
        Some(&mut attr_taint),
    )?;
    Ok(SubstitutionResult {
        css: v.css,
        attr_taint,
    })
}
