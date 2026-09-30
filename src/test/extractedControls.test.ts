import { describe, expect, it } from 'vitest';
import {
  ADJUSTMENT_SECTIONS,
  COPYABLE_ADJUSTMENT_KEYS,
  INITIAL_ADJUSTMENTS,
  normalizeLoadedAdjustments,
} from '../utils/adjustments';

describe('extracted development controls in RapidRAW', () => {
  it('registers Levels, Black and White, and Vignetting for section reset and copy', () => {
    expect(ADJUSTMENT_SECTIONS.levels).toEqual(['levels']);
    expect(ADJUSTMENT_SECTIONS.blackWhite).toEqual(['blackWhiteEnabled', 'blackWhiteMix']);
    expect(ADJUSTMENT_SECTIONS.vignetting).toEqual(['vignetting']);
    for (const key of ['levels', 'blackWhiteEnabled', 'blackWhiteMix', 'vignetting']) {
      expect(COPYABLE_ADJUSTMENT_KEYS).toContain(key);
    }
  });

  it('fills missing nested channel and vignetting defaults in older adjustments', () => {
    const loaded = normalizeLoadedAdjustments({
      ...INITIAL_ADJUSTMENTS,
      levels: { rgb: { inputBlack: 12 } },
      vignetting: { amount: -30 },
      blackWhiteMix: [10],
    } as unknown as typeof INITIAL_ADJUSTMENTS);

    expect(loaded.levels.rgb).toMatchObject({ inputBlack: 12, inputWhite: 255, outputBlack: 0 });
    expect(loaded.levels.red).toEqual(INITIAL_ADJUSTMENTS.levels.red);
    expect(loaded.vignetting).toEqual({ enabled: true, amount: -30, method: 'ellipticOnCrop' });
    expect(loaded.blackWhiteMix).toEqual([10, 0, 0, 0, 0, 0, 0, 0]);
  });
});
