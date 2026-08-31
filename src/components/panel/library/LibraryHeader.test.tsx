import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '../../../i18n';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { useLibraryStore } from '../../../store/useLibraryStore';
import {
  EditedStatus,
  LibraryViewMode,
  RawStatus,
  Theme,
  ThumbnailAspectRatio,
  ThumbnailSize,
} from '../../ui/AppProperties';
import { ViewOptionsDropdown } from './LibraryHeader';

const invoke = vi.fn().mockResolvedValue(undefined);
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

const renderViewOptions = () =>
  render(
    <ViewOptionsDropdown
      libraryViewMode={LibraryViewMode.Flat}
      onSelectSize={() => {}}
      onSelectAspectRatio={() => {}}
      setLibraryViewMode={() => {}}
      thumbnailSize={ThumbnailSize.Medium}
      thumbnailAspectRatio={ThumbnailAspectRatio.Cover}
      thumbnailSizeOptions={[{ id: ThumbnailSize.Medium, label: 'Medium', size: 240 }]}
      thumbnailAspectRatioOptions={[{ id: ThumbnailAspectRatio.Cover, label: 'Cover' }]}
      ratingFilterOptions={[{ value: 0, label: 'All' }]}
      rawStatusOptions={[{ key: RawStatus.All, label: 'All' }]}
      editedStatusOptions={[{ key: EditedStatus.All, label: 'All' }]}
      sortOptions={[
        { key: 'name', label: 'Name' },
        { key: 'date_taken', label: 'Date Taken' },
      ]}
    />,
  );

describe('capture-session controls', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockClear();
    useSettingsStore.setState({
      appSettings: {
        lastRootPath: null,
        theme: Theme.Dark,
        captureTimeGroupingEnabled: false,
        captureTimeGroupingSeconds: 900,
      },
    });
    useLibraryStore.setState({
      sortCriteria: { key: 'name', order: 'desc' },
      filterCriteria: { rating: 0, rawStatus: RawStatus.All, colors: [] },
    });
  });

  afterEach(() => vi.useRealTimers());

  it('updates the view immediately and debounces persisted slider writes', () => {
    const { container } = renderViewOptions();

    fireEvent.click(container.querySelector('[data-tooltip="View Options"]')!);
    fireEvent.click(screen.getByRole('checkbox', { name: 'Group by capture time' }));
    expect(useSettingsStore.getState().appSettings?.captureTimeGroupingEnabled).toBe(true);

    const slider = screen.getByRole('slider', { name: 'Capture session gap: 15 minutes' });
    fireEvent.change(slider, { target: { value: '0' } });
    expect(useSettingsStore.getState().appSettings?.captureTimeGroupingSeconds).toBe(3);
    expect(screen.getByRole('slider', { name: 'Capture session gap: 3 seconds' })).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalled();

    vi.advanceTimersByTime(350);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1]).toMatchObject({
      settings: { captureTimeGroupingEnabled: true, captureTimeGroupingSeconds: 3 },
    });
  });

  it('falls back to legacy minute settings when no seconds value is stored', () => {
    useSettingsStore.setState({
      appSettings: {
        lastRootPath: null,
        theme: Theme.Dark,
        captureTimeGroupingEnabled: true,
        captureTimeGroupingMinutes: 42,
      },
    });
    const { container } = renderViewOptions();

    fireEvent.click(container.querySelector('[data-tooltip="View Options"]')!);
    expect(screen.getByRole('slider', { name: 'Capture session gap: 42 minutes' })).toBeInTheDocument();
  });
});
