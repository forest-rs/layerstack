// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A model's draw mode stand-in: the axes, box or textured cards Hydra
//! draws in place of a model whose `model:drawMode` is `origin`, `bounds`
//! or `cards` (`UsdGeomModelAPI`).
//!
//! The geometry is OpenUSD 26.11's `UsdImaging_DrawModeStandin`
//! (`pxr/usdImaging/usdImaging/drawModeStandin.cpp`), in the model's own
//! space:
//!
//! - **origin**: three unit line segments from the origin along `+X`, `+Y`
//!   and `+Z` (`_OriginDrawMode`);
//! - **bounds**: the twelve edges of the extent's box (`_BoundsDrawMode`),
//!   the bottom face, the top face, then the vertical edges;
//! - **cards**: up to six quads (`_CardsDrawMode`). With `box` they are the
//!   faces of the extent's box; with `cross` the three planes through its
//!   center, each drawn twice facing opposite ways (a gap of 2⁻²³ of the
//!   box apart, as Hydra keeps them from being coplanar). A face is drawn
//!   when it or the opposite face has a texture (`model:cardTexture*`);
//!   a face without one shows the opposite face's texture, mirrored. When
//!   no face has a texture, all six are drawn in `model:drawModeColor`.
//!   `model:cardVisibility = "simple"` hides the two cards facing the
//!   stage's up axis.
//!
//! Cards are drawn one-sided (Hydra culls their backs): each quad winds
//! counter-clockwise seen from the side it faces, and is drawn as the
//! triangles `0 1 2` and `0 2 3` of its corners.

use alloc::vec::Vec;

use crate::shapes::Axis;

/// The alternate imaging mode a model is drawn with
/// (`UsdGeomModelAPI` `model:drawMode`), when it is not drawn as itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrawMode {
    /// Axes at the model's origin.
    Origin,
    /// The edges of the model's bounding box.
    Bounds,
    /// Textured quads on or through the model's bounding box.
    Cards,
}

impl DrawMode {
    /// The stand-in mode a resolved `model:drawMode` token names; `None`
    /// for `default`, `inherited` or a token Hydra does not draw
    /// (`UsdImaging_GetDrawModeStandin` makes no stand-in for them).
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "origin" => Self::Origin,
            "bounds" => Self::Bounds,
            "cards" => Self::Cards,
            _ => return None,
        })
    }

    /// The mode's token.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Bounds => "bounds",
            Self::Cards => "cards",
        }
    }
}

/// The geometry of a model's cards (`model:cardGeometry`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CardGeometry {
    /// Three planes through the box's center: the schema's fallback, and
    /// what a token the schema does not list draws.
    #[default]
    Cross,
    /// The six faces of the box.
    Box,
    /// Quads placed by each texture's `worldtoscreen` image metadata.
    /// Stagehand reads no image, so draws no card for it: what Hydra draws
    /// for textures without that metadata.
    FromTexture,
}

impl CardGeometry {
    /// The geometry a `model:cardGeometry` token names: `box`,
    /// `fromTexture`, and otherwise `cross`.
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "box" => Self::Box,
            "fromTexture" => Self::FromTexture,
            _ => Self::Cross,
        }
    }
}

/// One face of a model's box, a card and its texture
/// (`model:cardTexture{X,Y,Z}{Pos,Neg}`), in Hydra's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CardFace {
    /// Facing `+X`.
    XPos,
    /// Facing `+Y`.
    YPos,
    /// Facing `+Z`.
    ZPos,
    /// Facing `-X`.
    XNeg,
    /// Facing `-Y`.
    YNeg,
    /// Facing `-Z`.
    ZNeg,
}

impl CardFace {
    /// Every face, in the order of [`StandinInput::textures`].
    pub const ALL: [Self; 6] = [
        Self::XPos,
        Self::YPos,
        Self::ZPos,
        Self::XNeg,
        Self::YNeg,
        Self::ZNeg,
    ];

    /// Its position in [`Self::ALL`].
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }

    /// The face opposite it.
    #[must_use]
    pub fn opposite(self) -> Self {
        Self::ALL[(self.index() + 3) % 6]
    }

    /// The attribute naming its texture, `model:cardTextureXPos` and so on.
    #[must_use]
    pub fn texture_attribute(self) -> &'static str {
        match self {
            Self::XPos => "model:cardTextureXPos",
            Self::YPos => "model:cardTextureYPos",
            Self::ZPos => "model:cardTextureZPos",
            Self::XNeg => "model:cardTextureXNeg",
            Self::YNeg => "model:cardTextureYNeg",
            Self::ZNeg => "model:cardTextureZNeg",
        }
    }
}

/// What a stand-in is built from: the model's draw mode, extent and card
/// settings, as resolved off the stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandinInput {
    /// The mode drawn.
    pub mode: DrawMode,
    /// The model's extent, `[min, max]` in its own space; `None` draws the
    /// box and cards at the origin with no size, as Hydra does for a
    /// model with neither `extent` nor `extentsHint`.
    pub extent: Option<[[f32; 3]; 2]>,
    /// The cards' geometry.
    pub card_geometry: CardGeometry,
    /// Whether each face, in [`CardFace::ALL`] order, has a texture: a
    /// non-empty asset path authored.
    pub textures: [bool; 6],
    /// The axis whose two cards are hidden: the stage's up axis when
    /// `model:cardVisibility` resolves to `simple` (`Y` or `Z`; Hydra
    /// hides none for `X`).
    pub hidden_axis: Option<Axis>,
}

/// One card: a quad in the model's space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Card {
    /// The face it is.
    pub face: CardFace,
    /// Its corners, counter-clockwise seen from the side it faces.
    pub corners: [[f32; 3]; 4],
    /// Each corner's texture coordinate (Hydra's `cardsUv`).
    pub uvs: [[f32; 2]; 4],
    /// The face whose texture it shows: its own, or the opposite face's
    /// when it has none. `None` when no face has a texture: it is drawn in
    /// `model:drawModeColor`.
    pub texture: Option<CardFace>,
}

/// A model's stand-in geometry, in its own space.
#[derive(Clone, Debug, PartialEq)]
pub struct Standin {
    /// The mode drawn.
    pub mode: DrawMode,
    /// Line segments, two endpoints each: the axes of `origin`, the box
    /// edges of `bounds`; empty for `cards`.
    pub lines: Vec<[f32; 3]>,
    /// The cards of `cards`, in Hydra's order (`+X`, `-X`, `+Y`, `-Y`,
    /// `+Z`, `-Z`, those drawn); empty otherwise.
    pub cards: Vec<Card>,
}

/// The stand-in Hydra draws for `input`.
///
/// OpenUSD 26.11: `pxr/usdImaging/usdImaging/drawModeStandin.cpp`
/// (`_ComputeOriginTopology`, `_ComputeBoundsTopology` and
/// `_BoundsPointsPrimvarValueDataSource`, and `_CardsDataCache`'s
/// `_SchemaValues`, `_ComputePoints`, `_ComputeUVs` and
/// `_ComputeGeomSubsets` with `_CardsPointsPrimvarValueDataSource`).
#[must_use]
pub fn draw_mode_standin(input: &StandinInput) -> Standin {
    let [min, max] = input.extent.unwrap_or([[0.0; 3]; 2]);
    let mut standin = Standin {
        mode: input.mode,
        lines: Vec::new(),
        cards: Vec::new(),
    };
    match input.mode {
        DrawMode::Origin => {
            let origin = [0.0; 3];
            for axis in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
                standin.lines.extend([origin, axis]);
            }
        }
        DrawMode::Bounds => {
            let ends = [min, max];
            let corner = |i: usize| [ends[i >> 2][0], ends[(i >> 1) & 1][1], ends[i & 1][2]];
            const EDGES: [usize; 24] = [
                0, 4, 4, 6, 6, 2, 2, 0, // bottom face
                1, 5, 5, 7, 7, 3, 3, 1, // top face
                0, 1, 4, 5, 6, 7, 2, 3, // vertical edges
            ];
            standin.lines.extend(EDGES.iter().map(|&i| corner(i)));
        }
        DrawMode::Cards => standin.cards = cards(input, min, max),
    }
    standin
}

/// The cards of `input`, placed in the box `min`..`max`.
fn cards(input: &StandinInput, min: [f32; 3], max: [f32; 3]) -> Vec<Card> {
    let has_texture = input.textures;
    let mut has_face = [false; 6];
    if input.card_geometry == CardGeometry::FromTexture {
        // Placed by image metadata stagehand does not read: Hydra draws no
        // card for a texture without it.
        return Vec::new();
    }
    for k in 0..6 {
        if has_texture[k] {
            // A face with a texture draws the opposite face too.
            has_face[k] = true;
            has_face[(k + 3) % 6] = true;
        }
    }
    let textured = has_texture.contains(&true);
    if !textured {
        has_face = [true; 6];
    }
    let mut has_texture = has_texture;
    match input.hidden_axis {
        Some(Axis::Y) => {
            for k in [1, 4] {
                has_face[k] = false;
                has_texture[k] = false;
            }
        }
        Some(Axis::Z) => {
            for k in [2, 5] {
                has_face[k] = false;
                has_texture[k] = false;
            }
        }
        Some(Axis::X) | None => {}
    }
    let textured = textured && has_texture.contains(&true);

    // A quad of the unit cube facing +X: on the face for `box`, through
    // the center for `cross`, which keeps the two cards of an axis 2^-23
    // apart.
    let x = if input.card_geometry == CardGeometry::Box {
        1.0
    } else {
        0.5
    };
    let eps = if input.card_geometry == CardGeometry::Cross {
        f32::EPSILON
    } else {
        0.0
    };
    let quad = [[x, 1.0, 1.0], [x, 0.0, 1.0], [x, 0.0, 0.0], [x, 1.0, 0.0]];
    // Turns the +X quad to face axis `i`: a quarter turn about Z for Y, a
    // turn about the diagonal for Z.
    let turn = |[a, b, c]: [f32; 3], i: usize| match i {
        0 => [a, b, c],
        1 => [1.0 - b, a, c],
        _ => [b, c, a],
    };
    let nudge = |[a, b, c]: [f32; 3], i: usize| {
        let mut p = [a, b, c];
        p[i] += eps;
        p
    };
    let place = |p: [f32; 3]| {
        core::array::from_fn(|axis| min[axis] * (1.0 - p[axis]) + max[axis] * p[axis])
    };
    let uvs = |flip_u: bool, flip_v: bool| {
        let uv = |u: f32, v: f32| {
            [
                if flip_u { 1.0 - u } else { u },
                if flip_v { 1.0 - v } else { v },
            ]
        };
        [uv(1.0, 1.0), uv(0.0, 1.0), uv(0.0, 0.0), uv(1.0, 0.0)]
    };

    let mut cards = Vec::new();
    for i in 0..3 {
        for k in [i, i + 3] {
            if !has_face[k] {
                continue;
            }
            let corners: [[f32; 3]; 4] = core::array::from_fn(|c| {
                if k < 3 {
                    place(turn(nudge(quad[c], i), i))
                } else {
                    // The opposite face: the point reflection through the
                    // box's center, corners reversed.
                    let p = turn(nudge(quad[3 - c], i), i);
                    place([1.0 - p[0], 1.0 - p[1], 1.0 - p[2]])
                }
            });
            // A face without its own texture shows the opposite face's,
            // mirrored; Z is mirrored in v for +Z, and in u for -Z.
            let uvs = match k {
                2 => uvs(false, !has_texture[2]),
                5 => uvs(true, has_texture[5]),
                _ => uvs(!has_texture[k], false),
            };
            let texture =
                textured.then(|| CardFace::ALL[if has_texture[k] { k } else { (k + 3) % 6 }]);
            cards.push(Card {
                face: CardFace::ALL[k],
                corners,
                uvs,
                texture,
            });
        }
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::{CardFace, CardGeometry, DrawMode, StandinInput, draw_mode_standin};
    use crate::shapes::Axis;

    fn input(mode: DrawMode) -> StandinInput {
        StandinInput {
            mode,
            extent: Some([[-1.0, 0.0, -2.0], [1.0, 4.0, 2.0]]),
            card_geometry: CardGeometry::Cross,
            textures: [false; 6],
            hidden_axis: None,
        }
    }

    #[test]
    fn origin_is_three_unit_axes() {
        let standin = draw_mode_standin(&input(DrawMode::Origin));
        assert_eq!(
            standin.lines,
            [
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 1.0]
            ]
        );
        assert!(standin.cards.is_empty());
    }

    #[test]
    fn bounds_are_the_twelve_box_edges() {
        let standin = draw_mode_standin(&input(DrawMode::Bounds));
        assert_eq!(standin.lines.len(), 24);
        // Hydra's first segment runs from (-x, -y, -z) to (+x, -y, -z).
        assert_eq!(standin.lines[0], [-1.0, 0.0, -2.0]);
        assert_eq!(standin.lines[1], [1.0, 0.0, -2.0]);
        // Every edge is axis-aligned and spans the extent on one axis.
        for edge in standin.lines.chunks(2) {
            let changed = (0..3).filter(|&a| edge[0][a] != edge[1][a]).count();
            assert_eq!(changed, 1);
        }
        // No extent: a box of no size at the origin.
        let mut none = input(DrawMode::Bounds);
        none.extent = None;
        assert!(
            draw_mode_standin(&none)
                .lines
                .iter()
                .all(|p| *p == [0.0; 3])
        );
    }

    /// The outward normal of a card's first triangle.
    fn normal(corners: &[[f32; 3]; 4]) -> [f32; 3] {
        let [a, b, c] = [corners[0], corners[1], corners[2]];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        n.map(|v| {
            if v > 0.0 {
                1.0
            } else if v < 0.0 {
                -1.0
            } else {
                0.0
            }
        })
    }

    #[test]
    fn untextured_box_cards_are_six_outward_faces_in_color() {
        let mut box_input = input(DrawMode::Cards);
        box_input.card_geometry = CardGeometry::Box;
        let standin = draw_mode_standin(&box_input);
        let faces: alloc::vec::Vec<CardFace> = standin.cards.iter().map(|c| c.face).collect();
        assert_eq!(
            faces,
            [
                CardFace::XPos,
                CardFace::XNeg,
                CardFace::YPos,
                CardFace::YNeg,
                CardFace::ZPos,
                CardFace::ZNeg
            ]
        );
        let expected = [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        for (card, n) in standin.cards.iter().zip(expected) {
            assert_eq!(card.texture, None);
            // Wound counter-clockwise seen from outside.
            assert_eq!(normal(&card.corners), n, "{:?}", card.face);
        }
        // The +X face lies on the box's max x.
        assert!(standin.cards[0].corners.iter().all(|p| p[0] == 1.0));
        assert_eq!(standin.cards[0].corners[0], [1.0, 4.0, 2.0]);
    }

    #[test]
    fn cross_cards_pass_through_the_center_slightly_apart() {
        let standin = draw_mode_standin(&input(DrawMode::Cards));
        assert_eq!(standin.cards.len(), 6);
        let plus = standin.cards[0].corners[0][0];
        let minus = standin.cards[1].corners[0][0];
        assert!(plus > 0.0 && plus < 1e-5, "{plus}");
        assert!(minus < 0.0 && minus > -1e-5, "{minus}");
    }

    #[test]
    fn a_textured_face_draws_its_opposite_mirrored() {
        let mut one = input(DrawMode::Cards);
        one.textures[CardFace::XPos.index()] = true;
        let standin = draw_mode_standin(&one);
        assert_eq!(standin.cards.len(), 2);
        assert_eq!(standin.cards[0].face, CardFace::XPos);
        assert_eq!(standin.cards[0].texture, Some(CardFace::XPos));
        assert_eq!(standin.cards[0].uvs[0], [1.0, 1.0]);
        assert_eq!(standin.cards[1].face, CardFace::XNeg);
        assert_eq!(standin.cards[1].texture, Some(CardFace::XPos));
        assert_eq!(standin.cards[1].uvs[0], [0.0, 1.0]);
        // Z: +Z mirrored in v, -Z in u when textured.
        let mut z = input(DrawMode::Cards);
        z.textures[CardFace::ZNeg.index()] = true;
        let standin = draw_mode_standin(&z);
        assert_eq!(standin.cards[0].face, CardFace::ZPos);
        assert_eq!(standin.cards[0].uvs[0], [1.0, 0.0]);
        assert_eq!(standin.cards[1].face, CardFace::ZNeg);
        assert_eq!(standin.cards[1].uvs[0], [0.0, 0.0]);
    }

    #[test]
    fn simple_visibility_hides_the_up_axis_cards() {
        let mut simple = input(DrawMode::Cards);
        simple.textures = [true; 6];
        simple.hidden_axis = Some(Axis::Y);
        let faces: alloc::vec::Vec<CardFace> = draw_mode_standin(&simple)
            .cards
            .iter()
            .map(|c| c.face)
            .collect();
        assert_eq!(
            faces,
            [
                CardFace::XPos,
                CardFace::XNeg,
                CardFace::ZPos,
                CardFace::ZNeg
            ]
        );
        simple.hidden_axis = Some(Axis::X);
        assert_eq!(draw_mode_standin(&simple).cards.len(), 6);
    }

    #[test]
    fn from_texture_draws_nothing_without_image_metadata() {
        let mut from = input(DrawMode::Cards);
        from.card_geometry = CardGeometry::FromTexture;
        from.textures = [true; 6];
        assert!(draw_mode_standin(&from).cards.is_empty());
    }
}
