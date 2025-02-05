# Parser Transformation Pipeline Documentation
This document describes the workflow and math behind the current text extraction parser. The goal is to show exactly how text coordinates, glyph metrics, and transformation matrices are combined, then to discuss why the resulting Y‑coordinate transformation looks nontraditional and what might be missing.

## 1. Overview
The parser processes PDF text content by:
- **Decoding the content stream into text/glyph tokens.**
- **Maintaining a current text state that includes:**
  - A text matrix that defines the transformation from "text space" (the coordinates where glyph metrics are defined) to an intermediate space.
  - The content stream defines a graphics state which is implicitly a stack. The parser maintains a stack of graphics states.
  - A CTM (Current Transformation Matrix) that maps this intermediate space into user or device space.

The final goal is to compute accurate positions of each glyph (and its bounding box) in device space.

## 2. Text Parsing Process
The parser's high‑level process is as follows:

### Content Extraction:
- The PDF's content stream is parsed token by token.
- When a text‑operator (such as `Tj` or `TJ`) appears, the parser switches into text mode.
- The parser keeps track of the active font, text state, and the appropriate transformation matrices.

### Maintaining the Text State:
- The active text matrix (often specified with the `Tm` command) holds information about scale, rotation, and translation in text space.
- The CTM represents the overall transformation from user space to device space.
- Both matrices are stored as 6‑element arrays representing a 2×3 affine transform.

### Glyph Decoding:
- Glyph codes are mapped to characters.
- Font metrics (advance, ascent, and descent) are associated with each glyph.
- Positions for each glyph in text space are computed according to the text state.

---

## 3. The Transformation Pipeline
The key routine in your parser is the transformation function (named here `transform_point`):

```rust
fn transform_point(ctm: &[f32; 6], text_matrix: &[f32; 6], x: f32, y: f32) -> (f32, f32) {
    // 1. First apply the text matrix (affine transform for text space)
    let tx = text_matrix[0] * x + text_matrix[2] * y + text_matrix[4];
    let ty = text_matrix[1] * x + text_matrix[3] * y + text_matrix[5];

    // 2. Then apply the CTM scaling (and part of the translation)
    let px = ctm[0] * tx + ctm[2] * ty + ctm[4];
    let py = ctm[1] * tx + ctm[3] * ty;
    
    // 3. Adjust the y coordinate to obtain a "user y"
    let user_y = -(ctm[5] - (py + ctm[5]));
    (px, user_y)
}
```

### What Happens in Each Step

**Text Matrix Application:**
- The initial glyph coordinate (x, y) in text space is transformed into an intermediate coordinate (tx, ty).
- This operation looks like a standard affine transformation:
  - _tx = A · x + C · y + E_
  - _ty = B · x + D · y + F_

**CTM Application:**
- Next, the CTM is applied to take the intermediate coordinate into device space.
- Notice that the CTM is applied in two parts:
  - **For X:**
    - `px = (CTM[0] · tx + CTM[2] · ty + CTM[4])`
  - **For Y:**
    - `py = (CTM[1] · tx + CTM[3] · ty)`
- **Observation:** The CTM translation component for Y (`CTM[5]`) is not applied in the same way as for X.

**Y‑Axis Adjustment:**
- The final Y‑coordinate is computed with the adjustment:
  - `user_y = -(ctm[5] - (py + ctm[5]));`
- **Interpretation:** This peculiar calculation is effectively "flipping" or otherwise offsetting the Y coordinate. In many PDFs the coordinate system has the Y‑axis flipped relative to the device's native coordinate system (bottom‑left vs. top‑left origin).
- The expression tries to compensate by using `CTM[5]` (the Y translation) in a non-conventional way. This is our first hint that the combination of CTM and text matrix translation might be incomplete or non‑uniform.

---

## 4. Discussion: What Might Be Missing?

### A. Trusting the Composite Transformation
In an ideal scenario the two matrices would be composed into one unified transform. That is, you would compute a composite matrix (say `T = CTM ⨉ TextMatrix`) that, when applied to `(x, y, 1)`, produces the correct device-space coordinate without any extra "hacks."

- **Current Approach:**
  - Your implementation applies the text matrix first, then applies the CTM (partially ignoring the CTM's Y translation) and finally "fixes" the Y value by subtracting from `CTM[5]` in a nonstandard way.
- **Possible Issue:**
  - The extra manipulation may indicate that the full CTM translation is not being handled symmetrically for both axes. This can be the source of discrepancies (especially noticeable at small font sizes).

### B. Handling of Y‑axis Orientation
PDFs often use a coordinate system where the Y‑axis is inverted relative to typical device coordinate systems (for example, bottom‑left versus top‑left). MuPDF's approach is to let the full text transform handle the flip by working with vector math directly.

- **In Your Parser:**
  - The way the Y value is computed (`user_y = -(ctm[5] - (py + ctm[5]))`) appears to be a workaround rather than the result of a complete matrix multiplication that accounts for a Y‑axis flip.
- **Insight:**
  - It suggests that the CTM translation (especially the Y-component) might not be fully integrated into the affine transformation. Essentially, you may be "fixing" the result after the fact rather than letting a properly composed transform do the work.

### C. A Unified Affine Transform
A potential fix is to compose the text matrix and CTM into a single affine transform that inherently handles scaling, rotation, reflection, and translation. In other words, if you calculate:
  `Composite_matrix = CTM ⨉ TextMatrix`
then apply this composite matrix to `(x, y, 1)` to get `(px, py)` in one step, you might avoid the need for an ad‑hoc Y correction.

---

## 5. Next Steps

1. **Examine the Composition:**
   - Consider rewriting the transformation process so that you multiply the CTM by the text matrix before any point transformation.
   - Verify that the composite matrix correctly reflects Y‑axis flipping if needed.
2. **Review the PDF Specification:**
   - Check the section on text rendering and coordinate transformations. The PDF spec explains that text matrices and CTMs work together and may require specific handling when the Y‑axis is inverted.
3. **Compare with MuPDF's Strategy:**
   - MuPDF applies full vector/matrix transformations to compute positions. If possible, follow their approach by handling both axes uniformly within a single composite transformation.
4. **Test with a Range of Font Sizes:**
   - Small fonts are more sensitive to transformation errors. Validate the new approach against small glyphs, ensuring that bounding boxes and glyph positions match expected values.

---

## 6. Conclusion
The fact that your current approach requires a "nonstandard" adjustment for the Y‑coordinate indicates that the CTM's translation (and potential Y‑axis inversion) isn't fully incorporated into the affine transformation. Rather than patching up the result afterward, composing the CTM and text matrix into a single transformation may yield more natural and correct device‑space coordinates—much like the method employed by MuPDF.

By documenting the exact flow as above and comparing the individual steps with a composite transformation approach, you should be better positioned to identify where the current method diverges from the expected behavior.
