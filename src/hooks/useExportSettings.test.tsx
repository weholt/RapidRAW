import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { ExportPreset, WatermarkAnchor } from '../components/ui/ExportImportProperties';
import { useExportSettings } from './useExportSettings';

function preset(overrides: Partial<ExportPreset> = {}): ExportPreset {
  return {
    id: 'preset-1',
    name: 'Preset 1',
    fileFormat: 'jpeg',
    jpegQuality: 90,
    enableResize: false,
    resizeMode: 'longEdge',
    resizeValue: 2048,
    dontEnlarge: true,
    keepMetadata: true,
    preserveTimestamps: false,
    stripGps: true,
    exportMasks: false,
    preserveFolders: false,
    filenameTemplate: '{original_filename}_edited',
    enableWatermark: false,
    watermarkPath: null,
    watermarkAnchor: WatermarkAnchor.BottomRight,
    watermarkScale: 10,
    watermarkSpacing: 5,
    watermarkOpacity: 75,
    ...overrides,
  };
}

describe('useExportSettings workflow selection', () => {
  it('starts with no workflow selection', () => {
    const { result } = renderHook(() => useExportSettings());
    expect(result.current.workflowIds).toEqual([]);
    expect(result.current.currentSettingsObject.workflowIds).toEqual([]);
  });

  it('applies workflow ids and their order from a preset', () => {
    const { result } = renderHook(() => useExportSettings());
    act(() => {
      result.current.handleApplyPreset(preset({ workflowIds: ['backup', 'receipt'] }));
    });
    expect(result.current.workflowIds).toEqual(['backup', 'receipt']);
    expect(result.current.currentSettingsObject.workflowIds).toEqual(['backup', 'receipt']);
  });

  it('clears the workflow selection when a preset carries none', () => {
    const { result } = renderHook(() => useExportSettings());
    act(() => {
      result.current.setWorkflowIds(['receipt']);
    });
    act(() => {
      result.current.handleApplyPreset(preset());
    });
    expect(result.current.workflowIds).toEqual([]);
  });

  it('updates the workflow selection and reflects it in the settings object', () => {
    const { result } = renderHook(() => useExportSettings());
    act(() => {
      result.current.setWorkflowIds(['receipt', 'backup']);
    });
    expect(result.current.currentSettingsObject.workflowIds).toEqual(['receipt', 'backup']);
  });
});
