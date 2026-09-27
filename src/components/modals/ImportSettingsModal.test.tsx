import { act, fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useUIStore } from '../../store/useUIStore';
import { Theme } from '../ui/AppProperties';
import type { ImportPreview } from '../ui/ExportImportProperties';
import ImportSettingsModal from './ImportSettingsModal';

const invoke = vi.fn();
const open = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: (...args: unknown[]) => open(...args),
}));

function preview(overrides: Partial<ImportPreview> = {}): ImportPreview {
  return {
    planId: 'plan-1',
    requestHash: 'hash-1',
    rows: [
      {
        sourcePath: 'C:/cards/a.nef',
        destinationRelativePath: '2026/a.nef',
        metadata: {},
        associatedFiles: [],
        byteSize: 12,
        status: 'ready',
        conflicts: [],
        warnings: [],
      },
    ],
    page: 0,
    pageSize: 200,
    totalRows: 1,
    hasMore: false,
    unsupportedFileCount: 2,
    warnings: [],
    errors: [],
    ...overrides,
  };
}

function startButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: 'modals.importSettings.startImport' });
}

async function settlePreview() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 450));
  });
}

describe('ImportSettingsModal preview-first execution', () => {
  beforeEach(() => {
    invoke.mockReset();
    open.mockReset();
    useSettingsStore.setState({
      appSettings: {
        lastRootPath: null,
        theme: Theme.Dark,
        importPresets: [
          {
            id: 'last-used',
            name: 'Last used',
            folderPattern: { version: 1, parts: [], missingTokenPolicy: 'empty' },
            filenamePattern: { version: 1, parts: [], missingTokenPolicy: 'empty' },
            operation: 'copy',
            collisionPolicy: 'renameWithSuffix',
            missingTokenPolicy: 'fallback',
            includeAssociatedFiles: true,
            preserveTimestamps: true,
            useCaptureTime: false,
          },
        ],
      },
      osPlatform: 'windows',
    });
    useUIStore.setState({ importSourcePaths: ['C:/cards/a.nef'], importTargetFolder: 'C:/library' });
    invoke.mockImplementation((command: string) => {
      if (command === 'create_import_plan') return Promise.resolve(preview());
      return Promise.resolve(undefined);
    });
  });

  it('debounces the authoritative preview and renders destination examples', async () => {
    render(<ImportSettingsModal fileCount={1} isOpen onClose={vi.fn()} onSave={vi.fn()} />);
    expect(invoke).not.toHaveBeenCalledWith('create_import_plan', expect.anything());
    await settlePreview();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('create_import_plan', expect.anything());
    const request = invoke.mock.calls[0][1].request;
    expect(request).toMatchObject({
      sourceFiles: ['C:/cards/a.nef'],
      destinationRoot: 'C:/library',
      operation: 'copy',
      collisionPolicy: 'renameWithSuffix',
    });
    expect(screen.getByRole('table')).toBeInTheDocument();
    expect(screen.getByText('C:/cards/a.nef')).toBeInTheDocument();
    expect(screen.getByText('2026/a.nef')).toBeInTheDocument();
    expect(screen.getByText('ready')).toBeInTheDocument();
  });

  it('keeps execution blocked with an explanation until the plan is free of blocking rows', async () => {
    invoke.mockImplementation((command: string) => {
      if (command === 'create_import_plan') {
        return Promise.resolve(
          preview({
            rows: [
              {
                ...preview().rows[0],
                status: 'blocked',
                conflicts: [
                  {
                    code: 'existingDestination',
                    message: 'Destination already exists',
                    conflictingSource: null,
                  },
                ],
              },
            ],
            errors: ['Resolve blocking preview conflicts before importing'],
          }),
        );
      }
      return Promise.resolve(undefined);
    });
    render(<ImportSettingsModal fileCount={1} isOpen onClose={vi.fn()} onSave={vi.fn()} />);
    await settlePreview();
    const blocked = startButton();
    expect(blocked).toBeDisabled();
    expect(blocked).toHaveAttribute('title', 'modals.importSettings.resolvePreviewErrors');
    expect(screen.getByText('Destination already exists')).toBeInTheDocument();
  });

  it('starts the import with the previewed plan identity and refreshes the reserved last-used preset', async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<ImportSettingsModal fileCount={1} isOpen onClose={vi.fn()} onSave={onSave} />);
    await settlePreview();
    expect(startButton()).toBeEnabled();
    await act(async () => {
      fireEvent.click(startButton());
    });
    expect(onSave).toHaveBeenCalledWith({ planId: 'plan-1', requestHash: 'hash-1' });
    const saved = invoke.mock.calls.find(([command]) => command === 'save_settings')?.[1] as {
      settings: { importPresets?: Array<{ id: string }> };
    };
    expect(saved.settings.importPresets?.some((preset) => preset.id === 'last-used')).toBe(true);
  });
});
