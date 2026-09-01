import { describe, expect, it } from 'vitest';
import {
  IMPORT_PATTERN_VERSION,
  ExportResultDetail,
  ImportPattern,
  ImportPlanRequest,
  ImportProgressEvent,
  WORKFLOW_DEFAULT_ORDER,
  WORKFLOW_DEFAULT_TIMEOUT_SECONDS,
  WORKFLOW_MAX_TIMEOUT_SECONDS,
  WORKFLOW_PROTOCOL_VERSION,
  WorkflowDiscoveryResult,
  WorkflowMetadata,
  WorkflowProgressEvent,
  WorkflowRefreshReport,
  WorkflowRequest,
} from '../components/ui/ExportImportProperties';

describe('workflow feature contracts', () => {
  it('represents an import plan and typed progress event', () => {
    const pattern: ImportPattern = {
      version: IMPORT_PATTERN_VERSION,
      parts: [{ type: 'token', token: 'originalStem', fallback: null }],
      missingTokenPolicy: 'error',
    };
    const request: ImportPlanRequest = {
      sourceFiles: ['source.nef'],
      sourceFolder: null,
      recursive: false,
      destinationRoot: 'library',
      folderPattern: { ...pattern, parts: [] },
      filenamePattern: pattern,
      operation: 'copy',
      collisionPolicy: 'renameWithSuffix',
      includeAssociatedFiles: true,
    };
    const event: ImportProgressEvent = {
      planId: 'plan-1',
      phase: 'copying',
      current: 1,
      total: 2,
      bytesCompleted: 1024,
      bytesTotal: 2048,
      sourcePath: 'source.nef',
    };

    expect(request.operation).toBe('copy');
    expect(event.phase).toBe('copying');
  });

  it('represents workflow discovery, requests, and progress without untyped payloads', () => {
    const discovery: WorkflowDiscoveryResult = {
      workflows: [
        {
          id: 'receipt',
          displayName: 'Receipt',
          description: null,
          language: 'javaScript',
          phase: 'postBatch',
          order: 0,
          timeoutSeconds: 30,
          onError: 'warn',
          source: 'user',
          scriptPath: 'C:/Users/photographer/.rapidraw/workflows/receipt.js',
          selectable: true,
          runtime: {
            available: true,
            executable: 'node',
            version: 'v24',
            unavailableReason: null,
          },
          diagnostics: [],
        },
      ],
      diagnostics: [],
    };
    const refresh: WorkflowRefreshReport = {
      result: discovery,
      addedWorkflowIds: ['receipt'],
      removedWorkflowIds: [],
      availabilityChanges: [],
    };
    const request: WorkflowRequest = {
      protocolVersion: WORKFLOW_PROTOCOL_VERSION,
      runId: 'run-1',
      workflowId: discovery.workflows[0]!.id,
      phase: 'postBatch',
      sourcePath: null,
      exportedPath: null,
      artifacts: [],
      selectedItems: [],
      exportedItems: [],
      exportSettings: {
        fileFormat: 'jpeg',
        jpegQuality: 90,
        keepMetadata: true,
        stripGps: false,
      },
      index: null,
      total: 0,
      workspaceTempDirectory: 'temp',
    };
    const event: WorkflowProgressEvent = {
      runId: request.runId,
      phase: 'runningPostBatch',
      workflowId: request.workflowId,
      sourcePath: null,
      index: 0,
      total: 1,
      timeoutSeconds: 30,
      warningCount: 0,
    };

    expect(request.protocolVersion).toBe(1);
    expect(event.runId).toBe(request.runId);
    expect(discovery.workflows[0]!.selectable).toBe(true);
    expect(refresh.result.workflows[0]!.id).toBe(request.workflowId);
    expect(refresh.availabilityChanges).toEqual([]);

    const rendering: WorkflowProgressEvent = { ...event, phase: 'rendering', workflowId: null };
    const writing: WorkflowProgressEvent = {
      ...event,
      phase: 'writing',
      workflowId: null,
      sourcePath: 'source.nef',
    };
    expect(rendering.phase).toBe('rendering');
    expect(writing.phase).toBe('writing');
  });

  it('represents the terminal export result detail with per-item and per-workflow outcomes', () => {
    const detail: ExportResultDetail = {
      runId: 'run-1',
      cancelled: false,
      total: 2,
      succeeded: 1,
      failed: 1,
      warnedWorkflowRuns: 1,
      failedWorkflowRuns: 0,
      items: [
        { sourcePath: 'a.nef', exportedPath: 'a.jpg', error: null },
        { sourcePath: 'b.nef', exportedPath: null, error: 'render failed' },
      ],
      workflowRuns: [
        {
          workflowId: 'receipt',
          sourcePath: 'a.nef',
          status: 'warned',
          message: 'done with warnings',
          warnings: ['stale sidecar'],
          producedArtifacts: [],
          stderrExcerpt: '~/cache/receipt.log',
        },
      ],
    };

    expect(detail.items[1]!.error).toBe('render failed');
    expect(detail.workflowRuns[0]!.stderrExcerpt).toContain('~');
    expect(detail.warnedWorkflowRuns).toBe(1);
    expect(detail.failedWorkflowRuns).toBe(0);
  });

  it('represents optional workflow metadata sidecars with safe defaults', () => {
    const sidecar: WorkflowMetadata = {
      id: 'example-post-batch-receipt',
      displayName: 'Example post-batch receipt',
      description: 'Writes a receipt.json into the workspace temp directory after the batch settles.',
      phase: 'postBatch',
      order: WORKFLOW_DEFAULT_ORDER,
      timeoutSeconds: 30,
      onError: 'warn',
    };
    const directFile: WorkflowMetadata = {};

    expect(sidecar.phase).toBe('postBatch');
    expect(directFile.id).toBeUndefined();
    expect(directFile.phase).toBeUndefined();
    expect(WORKFLOW_DEFAULT_TIMEOUT_SECONDS).toBeLessThanOrEqual(WORKFLOW_MAX_TIMEOUT_SECONDS);
    expect(WORKFLOW_DEFAULT_ORDER).toBe(100);
  });
});
