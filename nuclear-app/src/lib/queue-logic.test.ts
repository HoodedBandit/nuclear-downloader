import { describe, expect, it } from 'vitest';
import {
  canStartWork,
  deriveSelectionState,
  isAudioOnlyFormat,
  isUpdateBlockingStatus,
  redactDiagnosticText,
  resolveAvailableFormat,
  resolveAvailableQuality,
  type QueueLifecycleStatus
} from './queue-logic';

describe('update safety gating', () => {
  it.each<QueueLifecycleStatus>([
    'fetching',
    'queued',
    'downloading',
    'postprocessing',
    'cancelling'
  ])('blocks updates while %s work can still own a child process', (status) => {
    expect(isUpdateBlockingStatus(status)).toBe(true);
  });

  it.each<QueueLifecycleStatus>(['ready', 'completed', 'error', 'cancelled'])(
    'allows updates for the inert %s state',
    (status) => {
      expect(isUpdateBlockingStatus(status)).toBe(false);
    }
  );
});

describe('quality selection', () => {
  it('falls back to best when a global quality is unavailable', () => {
    expect(resolveAvailableQuality('2160p', ['best', '1080p', '720p'])).toBe('best');
    expect(resolveAvailableQuality('1080p', ['best', '1080p', '720p'])).toBe('1080p');
  });
});

describe('format capability gating', () => {
  it('rejects audio-only output for a video inspected without audio', () => {
    expect(isAudioOnlyFormat('mp3')).toBe(true);
    expect(resolveAvailableFormat('mp3', false, 'mp4')).toBe('mp4');
    expect(resolveAvailableFormat('mp3', null, 'mp4')).toBe('mp3');
    expect(resolveAvailableFormat('mkv', false, 'mp4')).toBe('mkv');
  });
});

describe('selection state', () => {
  it('derives checked and indeterminate state without duplicated flags', () => {
    expect(deriveSelectionState([])).toBe('none');
    expect(deriveSelectionState([{ selected: false }, { selected: false }])).toBe('none');
    expect(deriveSelectionState([{ selected: true }, { selected: false }])).toBe('some');
    expect(deriveSelectionState([{ selected: true }, { selected: true }])).toBe('all');
  });
});

describe('work admission selector', () => {
  it('requires runtime, a validated destination, and no maintenance lease', () => {
    expect(
      canStartWork({
        runtimeReady: true,
        outputDirectoryReady: true,
        maintenanceActive: false
      })
    ).toBe(true);
    expect(
      canStartWork({
        runtimeReady: true,
        outputDirectoryReady: false,
        maintenanceActive: false
      })
    ).toBe(false);
    expect(
      canStartWork({
        runtimeReady: true,
        outputDirectoryReady: true,
        maintenanceActive: true
      })
    ).toBe(false);
  });
});

describe('diagnostic redaction', () => {
  it('removes URLs, absolute Windows paths, and cookie arguments', () => {
    expect(
      redactDiagnosticText(
        'source https://example.test/private?id=1\nfile C:\\Users\\Alice\\secret.mp4\n--cookies cookies.txt'
      )
    ).toBe('source [url redacted]\nfile [path redacted]\n--cookies [redacted]');
  });
});
