import { useState } from 'react';
import { act, render, renderHook, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useSettingsStore } from '../../store/useSettingsStore';
import { Theme } from '../ui/AppProperties';
import type { ImportPreset } from '../ui/ExportImportProperties';
import { useImportSettings } from '../../hooks/useImportSettings';
import ImportPresetsList from './ImportPresetsList';

const invoke = vi.fn().mockResolvedValue(undefined);
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock('uuid', () => ({ v4: () => 'generated-id' }));

const pattern = {
  version: 1,
  parts: [{ type: 'literal' as const, value: 'photos' }],
  missingTokenPolicy: 'empty' as const,
};
const base: Omit<ImportPreset, 'id' | 'name'> = {
  folderPattern: pattern,
  filenamePattern: pattern,
  operation: 'copy',
  collisionPolicy: 'renameWithSuffix',
  missingTokenPolicy: 'fallback',
  includeAssociatedFiles: true,
  preserveTimestamps: true,
  useCaptureTime: false,
  lastSourceLocation: null,
  lastTargetLocation: null,
};
const lastUsed: ImportPreset = { ...base, id: 'last-used', name: 'Last used' };
const wedding: ImportPreset = { ...base, id: 'wedding', name: 'Wedding' };

function persisted(): ImportPreset[] {
  const calls = invoke.mock.calls.filter(([command]) => command === 'save_settings');
  const settings = calls.at(-1)?.[1] as { settings: { importPresets?: ImportPreset[] } } | undefined;
  return settings?.settings?.importPresets ?? [];
}

describe('import presets', () => {
  beforeEach(() => {
    invoke.mockClear();
    useSettingsStore.setState({
      appSettings: {
        lastRootPath: null,
        theme: Theme.Dark,
        importPresets: [lastUsed, wedding],
      },
    });
  });

  it('applies, saves, renames, and deletes presets while protecting the reserved last-used entry', async () => {
    const user = userEvent.setup();
    const onApply = vi.fn();
    const onSave = vi.fn();
    const onRename = vi.fn();
    const onDelete = vi.fn();
    function Harness() {
      const [selectedId, setSelectedId] = useState('last-used');
      return (
        <ImportPresetsList
          presets={[lastUsed, wedding]}
          selectedId={selectedId}
          current={base}
          onApply={(preset) => {
            setSelectedId(preset.id);
            onApply(preset);
          }}
          onSave={onSave}
          onRename={onRename}
          onDelete={onDelete}
        />
      );
    }
    render(<Harness />);
    const select = screen.getByRole('combobox');
    expect(select).toHaveValue('last-used');
    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual(['Last used', 'Wedding']);

    await user.selectOptions(select, 'wedding');
    expect(onApply).toHaveBeenCalledWith(wedding);

    const name = screen.getByRole('textbox', { name: 'modals.importSettings.presetName' });
    expect(screen.getByRole('button', { name: 'modals.importSettings.savePreset' })).toBeDisabled();
    await user.type(name, 'Studio');
    await user.click(screen.getByRole('button', { name: 'modals.importSettings.renamePreset' }));
    expect(onRename).toHaveBeenCalledWith('wedding', 'Studio');
    await user.click(screen.getByRole('button', { name: 'modals.importSettings.savePreset' }));
    expect(onSave).toHaveBeenCalledWith('Studio', base);
    expect(name).toHaveValue('');
    await user.click(screen.getByRole('button', { name: 'modals.importSettings.deletePreset' }));
    expect(onDelete).toHaveBeenCalledWith('wedding');
  });

  it('disables rename and delete for the reserved last-used preset', () => {
    render(
      <ImportPresetsList
        presets={[lastUsed]}
        selectedId="last-used"
        current={base}
        onApply={vi.fn()}
        onSave={vi.fn()}
        onRename={vi.fn()}
        onDelete={vi.fn()}
      />,
    );
    expect(screen.getByRole('button', { name: 'modals.importSettings.renamePreset' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'modals.importSettings.deletePreset' })).toBeDisabled();
  });

  it('persists CRUD through the settings store without ever deleting the reserved entry', async () => {
    const { result } = renderHook(() => useImportSettings());
    expect(result.current.presets).toHaveLength(2);

    await act(async () => {
      await result.current.savePreset('Studio', base);
    });
    await waitFor(() => expect(persisted()).toHaveLength(3));
    expect(persisted().at(-1)).toMatchObject({ id: 'generated-id', name: 'Studio' });

    await act(async () => {
      await result.current.updatePreset('wedding', { name: 'Wedding 2026' });
    });
    await waitFor(() => expect(persisted().find((preset) => preset.id === 'wedding')?.name).toBe('Wedding 2026'));

    await act(async () => {
      await result.current.deletePreset('last-used');
    });
    expect(persisted().some((preset) => preset.id === 'last-used')).toBe(true);

    await act(async () => {
      await result.current.deletePreset('wedding');
    });
    await waitFor(() => expect(persisted().some((preset) => preset.id === 'wedding')).toBe(false));
  });
});
