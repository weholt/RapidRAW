import { describe, expect, it } from 'vitest';
import { Adjustments, INITIAL_ADJUSTMENTS, adjustmentsToRecipe, recipeToAdjustments } from '../utils/adjustments';

describe('extracted RAW development controls', () => {
  it('keeps Levels, Color Balance, black and white, and vignetting across the host recipe boundary', () => {
    const edited: Adjustments = {
      ...INITIAL_ADJUSTMENTS,
      levels: {
        ...INITIAL_ADJUSTMENTS.levels,
        enabled: true,
        rgb: { ...INITIAL_ADJUSTMENTS.levels.rgb, inputBlack: 12, inputWhite: 242 },
      },
      colorGrading: {
        ...INITIAL_ADJUSTMENTS.colorGrading,
        global: { hue: 275, saturation: 25, luminance: -10 },
      },
      blackWhiteEnabled: true,
      blackWhiteMix: [10, 0, 0, 0, 0, 0, 0, -10],
      vignetting: { enabled: true, amount: -1, method: 'circularOnCrop' },
    };

    const recipe = adjustmentsToRecipe(edited);
    const restored = recipeToAdjustments(recipe);

    expect(restored.levels).toEqual(edited.levels);
    expect(restored.colorGrading.global).toEqual(edited.colorGrading.global);
    expect(restored.blackWhiteEnabled).toBe(true);
    expect(restored.blackWhiteMix).toEqual(edited.blackWhiteMix);
    expect(restored.vignetting).toEqual(edited.vignetting);
  });
});
