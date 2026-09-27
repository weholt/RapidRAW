import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import ImportPatternBuilder from '../import/ImportPatternBuilder';
import ImportPreviewTable from '../import/ImportPreviewTable';
import ImportPresetsList from '../import/ImportPresetsList';
import ImportProgress from '../import/ImportProgress';
import ImportResult from '../import/ImportResult';
import { Invokes } from '../ui/AppProperties';
import type {
  CollisionPolicy,
  ExecuteImportRequest,
  ImportOperation,
  ImportPattern,
  ImportPlanRequest,
  ImportPreset,
  ImportPreview,
  MissingTokenPolicy,
} from '../ui/ExportImportProperties';
import { IMPORT_PATTERN_VERSION } from '../ui/ExportImportProperties';
import { useImportSettings } from '../../hooks/useImportSettings';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useUIStore } from '../../store/useUIStore';

interface Props {
  fileCount: number;
  isOpen: boolean;
  onClose(): void;
  onSave(request: ExecuteImportRequest): Promise<void>;
}

const EMPTY_FOLDER: ImportPattern = {
  version: IMPORT_PATTERN_VERSION,
  parts: [],
  missingTokenPolicy: 'empty',
};
const DEFAULT_FILENAME: ImportPattern = {
  version: IMPORT_PATTERN_VERSION,
  parts: [{ type: 'token', token: 'originalStem', fallback: 'image' }],
  missingTokenPolicy: 'fallback',
};

export default function ImportSettingsModal({ fileCount: _fileCount, isOpen, onClose, onSave }: Props) {
  const { t } = useTranslation();
  const { presets, savePreset, updatePreset, deletePreset } = useImportSettings();
  const { importSourcePaths, importTargetFolder } = useUIStore();
  const isAndroid = useSettingsStore((state) => state.osPlatform === 'android');
  const [sourceFolder, setSourceFolder] = useState<string | null>(null);
  const [recursive, setRecursive] = useState(false);
  const [folderPattern, setFolderPattern] = useState(EMPTY_FOLDER);
  const [filenamePattern, setFilenamePattern] = useState(DEFAULT_FILENAME);
  const [operation, setOperation] = useState<ImportOperation>('copy');
  const [collisionPolicy, setCollisionPolicy] = useState<CollisionPolicy>('renameWithSuffix');
  const [missingTokenPolicy, setMissingTokenPolicy] = useState<MissingTokenPolicy>('fallback');
  const [includeAssociatedFiles, setIncludeAssociatedFiles] = useState(true);
  const [preserveTimestamps, setPreserveTimestamps] = useState(true);
  const [useCaptureTime, setUseCaptureTime] = useState(false);
  const [selectedPresetId, setSelectedPresetId] = useState('last-used');
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [previewError, setPreviewError] = useState('');
  const [loading, setLoading] = useState(false);
  const generation = useRef(0);

  const request = useMemo<ImportPlanRequest | null>(() => {
    if (!importTargetFolder || (!sourceFolder && importSourcePaths.length === 0)) return null;
    return {
      sourceFiles: sourceFolder ? [] : importSourcePaths,
      sourceFolder,
      recursive,
      destinationRoot: importTargetFolder,
      folderPattern: { ...folderPattern, missingTokenPolicy },
      filenamePattern: { ...filenamePattern, missingTokenPolicy },
      operation: isAndroid ? 'copy' : operation,
      collisionPolicy,
      includeAssociatedFiles,
      preserveTimestamps,
      useCaptureTime,
    };
  }, [
    importSourcePaths,
    importTargetFolder,
    sourceFolder,
    recursive,
    folderPattern,
    filenamePattern,
    operation,
    collisionPolicy,
    missingTokenPolicy,
    includeAssociatedFiles,
    preserveTimestamps,
    useCaptureTime,
    isAndroid,
  ]);

  const regenerate = useCallback(async (next: ImportPlanRequest, currentGeneration: number) => {
    setLoading(true);
    setPreviewError('');
    try {
      const result = await invoke<ImportPreview>(Invokes.CreateImportPlan, { request: next });
      if (generation.current === currentGeneration) setPreview(result);
    } catch (error) {
      if (generation.current === currentGeneration) {
        setPreview(null);
        setPreviewError(String(error));
      }
    } finally {
      if (generation.current === currentGeneration) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!isOpen || !request) return;
    const currentGeneration = ++generation.current;
    setPreview(null);
    const timer = window.setTimeout(() => regenerate(request, currentGeneration), 350);
    return () => window.clearTimeout(timer);
  }, [isOpen, request, regenerate]);

  useEffect(() => {
    if (!isOpen) return;
    setSourceFolder(null);
    setRecursive(false);
    const last = presets.find((preset) => preset.id === 'last-used');
    if (last) applyPreset(last);
    // Restore only when the workspace opens; subsequent preset changes are user driven.
  }, [isOpen]);

  const currentPreset = useMemo<Omit<ImportPreset, 'id' | 'name'>>(
    () => ({
      folderPattern,
      filenamePattern,
      operation: isAndroid ? 'copy' : operation,
      collisionPolicy,
      missingTokenPolicy,
      includeAssociatedFiles,
      preserveTimestamps,
      useCaptureTime,
      lastSourceLocation: sourceFolder ?? importSourcePaths[0] ?? null,
      lastTargetLocation: importTargetFolder,
    }),
    [
      folderPattern,
      filenamePattern,
      operation,
      collisionPolicy,
      missingTokenPolicy,
      includeAssociatedFiles,
      preserveTimestamps,
      useCaptureTime,
      sourceFolder,
      importSourcePaths,
      importTargetFolder,
      isAndroid,
    ],
  );

  function applyPreset(preset: ImportPreset) {
    setSelectedPresetId(preset.id);
    setFolderPattern(preset.folderPattern);
    setFilenamePattern(preset.filenamePattern);
    setOperation(isAndroid ? 'copy' : preset.operation);
    setCollisionPolicy(preset.collisionPolicy);
    setMissingTokenPolicy(preset.missingTokenPolicy);
    setIncludeAssociatedFiles(preset.includeAssociatedFiles);
    setPreserveTimestamps(preset.preserveTimestamps);
    setUseCaptureTime(preset.useCaptureTime);
  }

  const blocked =
    !preview ||
    loading ||
    preview.errors.length > 0 ||
    preview.rows.some((row) => row.status === 'blocked') ||
    preview.totalRows === 0;

  if (!isOpen) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40" role="dialog" aria-modal="true">
      <div
        className="max-h-[95vh] w-[min(96vw,1000px)] space-y-4 overflow-auto rounded-lg bg-bg-primary p-5 shadow-xl"
        onKeyDown={(event) => {
          if (event.key === 'Escape') onClose();
        }}
      >
        <h2 className="text-lg font-semibold">{t('modals.importSettings.title')}</h2>
        <ImportPresetsList
          presets={presets}
          selectedId={selectedPresetId}
          current={currentPreset}
          onApply={applyPreset}
          onSave={savePreset}
          onRename={(id, name) => updatePreset(id, { name })}
          onDelete={deletePreset}
        />
        {!isAndroid && (
          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={async () => {
                const selected = await open({
                  directory: true,
                  multiple: false,
                  title: t('modals.importSettings.selectFolder'),
                });
                if (typeof selected === 'string') setSourceFolder(selected);
              }}
            >
              {t('modals.importSettings.chooseSourceFolder')}
            </button>
            {sourceFolder && <span className="truncate text-xs">{sourceFolder}</span>}
            <label>
              <input type="checkbox" checked={recursive} onChange={(event) => setRecursive(event.target.checked)} />
              {t('modals.importSettings.recursive')}
            </label>
          </div>
        )}
        <ImportPatternBuilder
          id="folder"
          label={t('modals.importSettings.folderPattern')}
          pattern={folderPattern}
          onChange={setFolderPattern}
        />
        <ImportPatternBuilder
          id="filename"
          label={t('modals.importSettings.filenamePattern')}
          pattern={filenamePattern}
          onChange={setFilenamePattern}
        />
        <div className="grid grid-cols-2 gap-3 text-sm md:grid-cols-4">
          {!isAndroid && (
            <label>
              {t('modals.importSettings.operation')}
              <select value={operation} onChange={(event) => setOperation(event.target.value as ImportOperation)}>
                <option value="copy">{t('modals.importSettings.copy')}</option>
                <option value="move">{t('modals.importSettings.move')}</option>
              </select>
            </label>
          )}
          <label>
            {t('modals.importSettings.collisionPolicy')}
            <select
              value={collisionPolicy}
              onChange={(event) => setCollisionPolicy(event.target.value as CollisionPolicy)}
            >
              <option value="renameWithSuffix">{t('modals.importSettings.renameWithSuffix')}</option>
              <option value="skip">{t('modals.importSettings.skip')}</option>
              <option value="error">{t('modals.importSettings.block')}</option>
            </select>
          </label>
          <label>
            {t('modals.importSettings.missingTokenPolicy')}
            <select
              value={missingTokenPolicy}
              onChange={(event) => setMissingTokenPolicy(event.target.value as MissingTokenPolicy)}
            >
              <option value="fallback">{t('modals.importSettings.fallback')}</option>
              <option value="empty">{t('modals.importSettings.empty')}</option>
              <option value="error">{t('modals.importSettings.block')}</option>
            </select>
          </label>
          <label>
            <input
              type="checkbox"
              checked={includeAssociatedFiles}
              onChange={(event) => setIncludeAssociatedFiles(event.target.checked)}
            />
            {t('modals.importSettings.includeSidecars')}
          </label>
          <label>
            <input
              type="checkbox"
              checked={preserveTimestamps}
              onChange={(event) => setPreserveTimestamps(event.target.checked)}
            />
            {t('modals.importSettings.preserveTimestamps')}
          </label>
          <label>
            <input
              type="checkbox"
              checked={useCaptureTime}
              disabled={preserveTimestamps}
              onChange={(event) => setUseCaptureTime(event.target.checked)}
            />
            {t('modals.importSettings.useCaptureTime')}
          </label>
        </div>
        {isAndroid && <p>{t('modals.importSettings.androidCopyOnly')}</p>}
        {previewError && (
          <p role="alert" className="text-red-400">
            {previewError}
          </p>
        )}
        <ImportPreviewTable
          preview={preview}
          loading={loading}
          onPage={async (page) => {
            if (!preview) return;
            setLoading(true);
            setPreviewError('');
            try {
              setPreview(
                await invoke<ImportPreview>(Invokes.GetImportPreviewPage, {
                  request: { planId: preview.planId, page, pageSize: preview.pageSize },
                }),
              );
            } catch (error) {
              setPreviewError(String(error));
            } finally {
              setLoading(false);
            }
          }}
        />
        <ImportProgress />
        <ImportResult />
        <div className="flex justify-end gap-3">
          <button type="button" onClick={onClose}>
            {t('modals.importSettings.cancel')}
          </button>
          <button
            type="button"
            disabled={blocked}
            title={blocked ? t('modals.importSettings.resolvePreviewErrors') : undefined}
            className="rounded bg-accent px-4 py-2 font-semibold disabled:opacity-50"
            onClick={async () => {
              if (!preview || blocked) return;
              await updatePreset('last-used', currentPreset);
              await onSave({ planId: preview.planId, requestHash: preview.requestHash });
            }}
          >
            {t('modals.importSettings.startImport')}
          </button>
        </div>
      </div>
    </div>
  );
}
