import { describe, expect, it } from 'vitest';
import { ImageFile, RawStatus, SortDirection } from '../components/ui/AppProperties';
import { computeGroupedLibrary } from './useSortedLibrary';

const makeImage = (path: string, capture: string, overrides: Partial<ImageFile> = {}): ImageFile => ({
  path,
  modified: Date.UTC(2024, 0, 1) / 1000,
  exif: { DateTimeOriginal: capture },
  is_edited: false,
  rating: 0,
  tags: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: false,
  group_id: null,
  ...overrides,
});

const derive = (
  imageList: ImageFile[],
  appSettings: Record<string, unknown> = {},
  searchCriteria = { tags: [] as string[], text: '', mode: 'OR' as const },
) =>
  computeGroupedLibrary(
    {
      imageList,
      imageRatings: {},
      filterCriteria: { rating: 0, rawStatus: RawStatus.All, colors: [] },
      searchCriteria,
      sortCriteria: { key: 'name', order: SortDirection.Descending },
    },
    {
      appSettings: {
        grouping: 'off',
        captureTimeGroupingEnabled: true,
        captureTimeGroupingMinutes: 15,
        ...appSettings,
      },
    },
  );

describe('capture sessions in the derived library', () => {
  it('filters and searches before sessionization and forces chronological image order', () => {
    const result = derive(
      [
        makeImage('/shoot/z.jpg', '2024:01:01 10:00:00', { tags: ['user:keep'] }),
        makeImage('/shoot/a.jpg', '2024:01:01 12:00:00'),
      ],
      {},
      { tags: [], text: 'z.jpg', mode: 'OR' },
    );

    expect(result.displayList.map((image) => image.path)).toEqual(['/shoot/z.jpg']);
    expect(result.captureSessions).toHaveLength(1);
    expect(result.captureSessions[0].images).toHaveLength(1);
  });

  it('collapses RAW/JPEG variants before creating headers without changing group IDs', () => {
    const result = derive(
      [
        makeImage('/shoot/a.raw', '2024:01:01 10:00:00', { is_raw: true, group_id: 'pair' }),
        makeImage('/shoot/a.jpg', '2024:01:01 10:00:01', { group_id: 'pair' }),
      ],
      { grouping: 'raw' },
    );

    expect(result.displayList.map((image) => image.path)).toEqual(['/shoot/a.raw']);
    expect(result.displayList[0].group_id).toBe('pair');
    expect(result.badges?.get('pair')?.count).toBe(2);
    expect(result.captureSessions[0].images).toHaveLength(1);
  });

  it('never merges recursive-folder sessions', () => {
    const result = derive([
      makeImage('/root/a/one.jpg', '2024:01:01 10:00:00'),
      makeImage('/root/b/two.jpg', '2024:01:01 10:01:00'),
    ]);
    expect(result.captureSessions.map((session) => session.folderPath)).toEqual(['/root/a', '/root/b']);
  });

  it('regroups deterministically when late EXIF replaces a modified-time fallback', () => {
    const first = makeImage('/shoot/one.jpg', '2024:01:01 10:00:00');
    const late = makeImage('/shoot/two.jpg', '2024:01:01 10:02:00');
    late.exif = null;
    late.modified = Date.UTC(2024, 0, 1, 12) / 1000;

    expect(derive([first, late]).captureSessions).toHaveLength(2);
    const enriched = { ...late, exif: { DateTimeOriginal: '2024:01:01 10:02:00' } };
    expect(derive([first, enriched]).captureSessions).toHaveLength(1);
  });

  it('leaves the saved user sort untouched while capture grouping is active', () => {
    const sortCriteria = { key: 'rating', order: SortDirection.Descending };
    const state = {
      imageList: [makeImage('/shoot/a.jpg', '2024:01:01 10:00:00')],
      imageRatings: {},
      filterCriteria: { rating: 0, rawStatus: RawStatus.All, colors: [] },
      searchCriteria: { tags: [], text: '', mode: 'OR' },
      sortCriteria,
    };
    computeGroupedLibrary(state, {
      appSettings: { grouping: 'off', captureTimeGroupingEnabled: true, captureTimeGroupingMinutes: 15 },
    });
    expect(state.sortCriteria).toBe(sortCriteria);
  });
});
