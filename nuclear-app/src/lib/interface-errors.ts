import type { ErrorSource } from './error-inbox';
import type { InspectionState } from './inspection-workflow';
import type { QueueActionState } from './queue-actions';
import type { RuntimeWorkflowState } from './runtime-workflow';
import { getQueueItemDisplayTitle, type QueuePresentationState } from './queue-presentation';

export function collectInterfaceErrors({
  inspectionState,
  queueActionState,
  runtimeState,
  themeError,
  queueState
}: {
  inspectionState: InspectionState;
  queueActionState: QueueActionState;
  runtimeState: RuntimeWorkflowState;
  themeError: string | null;
  queueState: QueuePresentationState;
}): ErrorSource[] {
  return [
    { key: 'link', context: 'Reading a link', detail: inspectionState.urlError },
    { key: 'queue', context: 'Updating downloads', detail: queueActionState.queueActionError },
    { key: 'cancel', context: 'Stopping downloads', detail: queueActionState.cancelAllError },
    {
      key: 'runtime',
      context: 'Download tools',
      detail: runtimeState.status?.state !== 'ready' ? runtimeState.status?.message : null
    },
    { key: 'appearance', context: 'Saving appearance', detail: themeError },
    ...queueState.items
      .filter((item) => item.error)
      .map((item) => ({
        key: 'download-' + item.id + '-' + item.downloadId,
        context: getQueueItemDisplayTitle(item),
        detail: [item.error, item.errorCode, item.errorDetail].filter(Boolean).join('\n')
      }))
  ];
}
