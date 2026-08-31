import { ImageFile } from '../components/ui/AppProperties';

export type CaptureTimeSource = 'dateTimeOriginal' | 'createDate' | 'modified';

export interface EffectiveCaptureTime {
  instantMs: number;
  source: CaptureTimeSource;
}

export interface CaptureSession {
  id: string;
  folderPath: string;
  startMs: number;
  endMs: number;
  fallbackCount: number;
  images: ImageFile[];
}

const EXIF_DATE_TIME = /^(\d{4}):(\d{2}):(\d{2})[ T](\d{2}):(\d{2}):(\d{2})$/;
const EXIF_OFFSET = /^([+-])(\d{2}):(\d{2})$/;

const parseExifDateTime = (value?: string, offset?: string, subseconds?: string): number | null => {
  if (!value) return null;
  const match = EXIF_DATE_TIME.exec(value.trim());
  if (!match) return null;

  const [, yearText, monthText, dayText, hourText, minuteText, secondText] = match;
  const year = Number(yearText);
  const month = Number(monthText);
  const day = Number(dayText);
  const hour = Number(hourText);
  const minute = Number(minuteText);
  const second = Number(secondText);
  if (month < 1 || month > 12 || day < 1 || hour > 23 || minute > 59 || second > 59) return null;

  const subsecondText = (subseconds ?? '').trim();
  if (subsecondText && !/^\d+$/.test(subsecondText)) return null;
  const milliseconds = Number((subsecondText + '000').slice(0, 3));
  const utc = Date.UTC(year, month - 1, day, hour, minute, second, milliseconds);
  const validation = new Date(utc);
  if (
    validation.getUTCFullYear() !== year ||
    validation.getUTCMonth() !== month - 1 ||
    validation.getUTCDate() !== day ||
    validation.getUTCHours() !== hour ||
    validation.getUTCMinutes() !== minute ||
    validation.getUTCSeconds() !== second
  ) {
    return null;
  }

  if (!offset) return utc;
  const offsetMatch = EXIF_OFFSET.exec(offset.trim());
  if (!offsetMatch) return null;
  const offsetHours = Number(offsetMatch[2]);
  const offsetMinutes = Number(offsetMatch[3]);
  if (offsetHours > 23 || offsetMinutes > 59) return null;
  const offsetMs = (offsetHours * 60 + offsetMinutes) * 60_000;
  return utc - (offsetMatch[1] === '+' ? offsetMs : -offsetMs);
};

const modifiedMilliseconds = (modified: number): number => {
  if (!Number.isFinite(modified)) return 0;
  return Math.abs(modified) < 100_000_000_000 ? modified * 1000 : modified;
};

export const captureInstantForImage = (image: ImageFile): EffectiveCaptureTime => {
  const exif = image.exif ?? {};
  const original = parseExifDateTime(exif.DateTimeOriginal, exif.OffsetTimeOriginal, exif.SubSecTimeOriginal);
  if (original !== null) return { instantMs: original, source: 'dateTimeOriginal' };

  const created = parseExifDateTime(exif.CreateDate, exif.OffsetTimeDigitized, exif.SubSecTimeDigitized);
  if (created !== null) return { instantMs: created, source: 'createDate' };

  return { instantMs: modifiedMilliseconds(image.modified), source: 'modified' };
};

export const normalizeCapturePath = (path: string): string =>
  path.split('?vc=')[0].replace(/\\/g, '/').replace(/\/+/g, '/').replace(/\/$/, '').toLocaleLowerCase('en-US');

export const captureFolderPath = (path: string): string => {
  const physicalPath = path.split('?vc=')[0].replace(/\\/g, '/').replace(/\/+/g, '/');
  const separatorIndex = physicalPath.lastIndexOf('/');
  return separatorIndex < 0 ? '' : physicalPath.slice(0, separatorIndex);
};

const stableHash = (value: string): string => {
  let hash = 0x811c9dc5;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(36);
};

export const groupImagesIntoCaptureSessions = (
  images: readonly ImageFile[],
  thresholdMinutes: number,
): CaptureSession[] => {
  const thresholdMs = Math.max(1, Math.min(120, Math.round(thresholdMinutes))) * 60_000;
  const folders = new Map<string, Array<{ image: ImageFile; capture: EffectiveCaptureTime; normalizedPath: string }>>();

  for (const image of images) {
    const folder = captureFolderPath(image.path);
    const folderKey = normalizeCapturePath(folder);
    const entries = folders.get(folderKey) ?? [];
    entries.push({ image, capture: captureInstantForImage(image), normalizedPath: normalizeCapturePath(image.path) });
    folders.set(folderKey, entries);
  }

  const sessions: CaptureSession[] = [];
  for (const folderKey of [...folders.keys()].sort()) {
    const entries = folders.get(folderKey)!;
    entries.sort(
      (left, right) =>
        left.capture.instantMs - right.capture.instantMs ||
        left.normalizedPath.localeCompare(right.normalizedPath) ||
        left.image.path.localeCompare(right.image.path),
    );

    let current: CaptureSession | null = null;
    let previousInstant = 0;
    for (const entry of entries) {
      if (!current || entry.capture.instantMs - previousInstant > thresholdMs) {
        current = {
          id: `capture-${stableHash(`${folderKey}\0${entry.normalizedPath}`)}`,
          folderPath: captureFolderPath(entry.image.path),
          startMs: entry.capture.instantMs,
          endMs: entry.capture.instantMs,
          fallbackCount: 0,
          images: [],
        };
        sessions.push(current);
      }
      current.images.push(entry.image);
      current.endMs = entry.capture.instantMs;
      if (entry.capture.source === 'modified') current.fallbackCount += 1;
      previousInstant = entry.capture.instantMs;
    }
  }

  return sessions;
};
