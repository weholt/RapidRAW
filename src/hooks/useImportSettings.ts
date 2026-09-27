import { useCallback } from 'react';
import { v4 as uuid } from 'uuid';
import type { ImportPreset } from '../components/ui/ExportImportProperties';
import { useSettingsStore } from '../store/useSettingsStore';

const NO_PRESETS: ImportPreset[] = [];

export function useImportSettings() {
  const presets = useSettingsStore((state) => state.appSettings?.importPresets ?? NO_PRESETS);

  const persist = useCallback(async (next: ImportPreset[]) => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (appSettings) await handleSettingsChange({ ...appSettings, importPresets: next });
  }, []);

  return {
    presets,
    savePreset: async (name: string, preset: Omit<ImportPreset, 'id' | 'name'>) =>
      persist([...presets, { ...preset, id: uuid(), name }]),
    updatePreset: async (id: string, update: Partial<ImportPreset>) =>
      persist(presets.map((preset) => (preset.id === id ? { ...preset, ...update } : preset))),
    deletePreset: async (id: string) => {
      if (id !== 'last-used') await persist(presets.filter((preset) => preset.id !== id));
    },
  };
}
