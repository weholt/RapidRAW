import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { ImportPreset } from '../ui/ExportImportProperties';

interface Props {
  presets: ImportPreset[];
  selectedId: string;
  current: Omit<ImportPreset, 'id' | 'name'>;
  onApply(preset: ImportPreset): void;
  onSave(name: string, preset: Omit<ImportPreset, 'id' | 'name'>): void;
  onRename(id: string, name: string): void;
  onDelete(id: string): void;
}

export default function ImportPresetsList({
  presets,
  selectedId,
  current,
  onApply,
  onSave,
  onRename,
  onDelete,
}: Props) {
  const { t } = useTranslation();
  const [name, setName] = useState('');
  return (
    <div className="flex flex-wrap items-center gap-2">
      <label>
        <span className="sr-only">{t('modals.importSettings.preset')}</span>
        <select
          value={selectedId}
          onChange={(event) => {
            const preset = presets.find((value) => value.id === event.target.value);
            if (preset) onApply(preset);
          }}
        >
          {presets.map((preset) => (
            <option value={preset.id} key={preset.id}>
              {preset.name}
            </option>
          ))}
        </select>
      </label>
      <input
        aria-label={t('modals.importSettings.presetName')}
        value={name}
        onChange={(event) => setName(event.target.value)}
      />
      <button
        type="button"
        disabled={!name.trim()}
        onClick={() => {
          onSave(name.trim(), current);
          setName('');
        }}
      >
        {t('modals.importSettings.savePreset')}
      </button>
      <button
        type="button"
        disabled={!name.trim() || selectedId === 'last-used'}
        onClick={() => onRename(selectedId, name.trim())}
      >
        {t('modals.importSettings.renamePreset')}
      </button>
      <button type="button" disabled={selectedId === 'last-used'} onClick={() => onDelete(selectedId)}>
        {t('modals.importSettings.deletePreset')}
      </button>
    </div>
  );
}
