/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Typed OM Numeric Declaration.

use crate::derives::*;
use crate::parser::{Parse, ParserContext};
use crate::typed_om::numeric_values::NoCalcNumeric;
use crate::values::generics::calc::CalcUnits;
use crate::values::specified::calc::{AllowParse, CalcNode};
use crate::values::specified::NoCalcLength;
use cssparser::{Parser, Token};
use style_traits::values::specified::AllowedNumericType;
use style_traits::{ParseError, StyleParseErrorKind};

/// A numeric declaration, with or without a `calc()` expression.
#[derive(Clone, ToTyped)]
pub enum NumericDeclaration {
    /// A numeric value without a `calc()` expression.
    NoCalc(NoCalcNumeric),

    /// A numeric value represented by a `calc()` expression.
    ///
    /// <https://drafts.csswg.org/css-values/#calc-notation>
    Calc(CalcNode),
}

impl Parse for NumericDeclaration {
    /// <https://drafts.css-houdini.org/css-typed-om-1/#dom-cssnumericvalue-parse>
    fn parse<'i, 't>(
        context: &ParserContext,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self, ParseError<'i>> {
        let location = input.current_source_location();

        // Step 1.
        let token = input.next()?;

        // Step 2.
        match *token {
            Token::Dimension {
                value, ref unit, ..
            } => {
                NoCalcLength::parse_dimension_with_context(context, value, unit)
                    .map(NoCalcNumeric::Length)
                    .map(Self::NoCalc)
                    .map_err(|()| location.new_unexpected_token_error(token.clone()))

                // TODO: Add support for other values.

                // Step 3.

                // TODO: A type should be created from unit and if that fails, the failure
                // should be propagated here.
            },

            Token::Function(ref name) => {
                let function = CalcNode::math_function(context, name, location)?;
                let allow_all_units = AllowParse::new(CalcUnits::ALL);
                let node = CalcNode::parse(context, input, function, allow_all_units)?;

                let allow_all_types = AllowedNumericType::All;
                let _ = node
                    .clone()
                    .into_length_or_percentage(allow_all_types)
                    .map_err(|()| {
                        location.new_custom_error(StyleParseErrorKind::UnspecifiedError)
                    })?;

                // TODO: Add support for other values represented by a `calc()` expression.

                Ok(Self::Calc(node))
            },

            ref token => return Err(location.new_unexpected_token_error(token.clone())),
        }
    }
}
