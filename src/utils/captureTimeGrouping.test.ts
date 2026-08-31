import { describe, expect, it } from 'vitest';
import { ImageFile } from '../components/ui/AppProperties';
import { captureInstantForImage, groupImagesIntoCaptureSessions } from './captureTimeGrouping';

const image = (
  path: string,
  dateTimeOriginal?: string,
  modified = Date.UTC(2024, 0, 1) / 1000,
  extraExif: Record<string, string> = {},
): ImageFile => ({
  path,
  modified,
  exif: dateTimeOriginal
    ? { DateTimeOriginal: dateTimeOriginal, ...extraExif }
    : Object.keys(extraExif).length
      ? extraExif
      : null,
  is_edited: false,
  rating: 0,
  tags: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: false,
  group_id: null,
});

describe('captureTimeGrouping', () => {
  it('parses EXIF timestamps explicitly and honors offset and subseconds', () => {
    const parsed = captureInstantForImage(
      image('offset.jpg', '2024:04:05 12:30:15', 0, {
        OffsetTimeOriginal: '+02:30',
        SubSecTimeOriginal: '125',
      }),
    );

    expect(parsed).toEqual({
      instantMs: Date.UTC(2024, 3, 5, 10, 0, 15, 125),
      source: 'dateTimeOriginal',
    });
  });

  it('rejects invalid EXIF dates and falls back through CreateDate to modified time', () => {
    expect(
      captureInstantForImage(
        image('created.jpg', '2024:02:30 10:00:00', 0, {
          CreateDate: '2024:02:29 10:00:00',
          OffsetTimeDigitized: '-01:00',
        }),
      ),
    ).toEqual({
      instantMs: Date.UTC(2024, 1, 29, 11),
      source: 'createDate',
    });
    expect(captureInstantForImage(image('fallback.jpg', undefined, 1_700_000_000))).toEqual({
      instantMs: 1_700_000_000_000,
      source: 'modified',
    });
  });

  it('uses adjacent gaps, keeps equality together, and supports chaining', () => {
    const input = [
      image('C:\\shoot\\c.jpg', '2024:01:01 10:30:01'),
      image('C:\\shoot\\a.jpg', '2024:01:01 10:00:00'),
      image('C:\\shoot\\b.jpg', '2024:01:01 10:15:00'),
      image('C:\\shoot\\d.jpg', '2024:01:01 10:45:00'),
    ];
    const original = [...input];
    const sessions = groupImagesIntoCaptureSessions(input, 15);

    expect(sessions.map((session) => session.images.map((item) => item.path))).toEqual([
      ['C:\\shoot\\a.jpg', 'C:\\shoot\\b.jpg'],
      ['C:\\shoot\\c.jpg', 'C:\\shoot\\d.jpg'],
    ]);
    expect(input).toEqual(original);
  });

  it('separates folders, counts fallbacks, and resolves ties by normalized path', () => {
    const sessions = groupImagesIntoCaptureSessions(
      [
        image('C:\\B\\same.jpg', undefined, 1_700_000_000),
        image('C:\\A\\z.jpg', '2024:01:01 10:00:00'),
        image('c:/a/A.jpg', '2024:01:01 10:00:00'),
      ],
      120,
    );

    expect(sessions).toHaveLength(2);
    expect(sessions[0].images.map((item) => item.path)).toEqual(['c:/a/A.jpg', 'C:\\A\\z.jpg']);
    expect(sessions[1].fallbackCount).toBe(1);
  });

  it('keeps IDs stable when an unrelated session is added', () => {
    const base = [image('/photos/a.jpg', '2024:01:01 10:00:00'), image('/photos/b.jpg', '2024:01:01 10:05:00')];
    const originalId = groupImagesIntoCaptureSessions(base, 15)[0].id;
    const withUnrelated = groupImagesIntoCaptureSessions(
      [...base, image('/photos/later.jpg', '2024:01:01 20:00:00')],
      15,
    );
    expect(withUnrelated[0].id).toBe(originalId);
  });

  it('groups 10,000 images within the interactive budget', () => {
    const input = Array.from({ length: 10_000 }, (_, index) =>
      image(`/large/${String(index).padStart(5, '0')}.jpg`, undefined, 1_700_000_000 + index * 60),
    );
    const started = performance.now();
    const sessions = groupImagesIntoCaptureSessions(input, 15);
    const elapsed = performance.now() - started;

    expect(sessions).toHaveLength(1);
    expect(sessions[0].images).toHaveLength(10_000);
    expect(elapsed).toBeLessThan(1_000);
  });
});
