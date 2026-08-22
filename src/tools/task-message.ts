import {
  type PluginInput,
  type ToolDefinition,
  tool,
} from '@opencode-ai/plugin';
import type { BackgroundJobStore } from '../utils/background-job-store';
import { getClient } from '../utils/opencode-client';
import { OperationTimeoutError, withTimeout } from '../utils/session';

const z = tool.schema;
const MAX_MESSAGE_LENGTH = 500;
const DEFAULT_MESSAGE_TIMEOUT_MS = 10_000;

class MessageLeaseOperationTimeoutError extends Error {
  constructor(
    message: string,
    readonly pending: boolean,
  ) {
    super(message);
    this.name = 'MessageLeaseOperationTimeoutError';
  }
}

export function createTaskMessageTool(options: {
  input: PluginInput;
  backgroundJobBoard: BackgroundJobStore;
  messageTimeoutMs?: number;
}): Record<'task_message', ToolDefinition> {
  const task_message = tool({
    description:
      'Queue a bounded message for a live child task without launching, resuming, or interrupting it.',
    args: {
      task_id: z
        .string()
        .describe('Tracked live task ID or parent-scoped alias'),
      message: z
        .string()
        .trim()
        .min(1)
        .max(MAX_MESSAGE_LENGTH)
        .describe('Short message to queue for the child task'),
    },
    async execute(args, toolContext) {
      const parentSessionID = toolContext?.sessionID;
      if (!parentSessionID) throw new Error('task_message requires sessionID');

      const requested = args.task_id.trim();
      const job = options.backgroundJobBoard.resolve(
        parentSessionID,
        requested,
      );
      if (!job) throw new Error(`Unknown task ID or alias: ${args.task_id}`);

      const currentJob = getCurrentTaskMessageJob(
        options.backgroundJobBoard,
        parentSessionID,
        requested,
        job.taskID,
        job.generation,
      );

      const lease = options.backgroundJobBoard.acquireMessageLease(
        currentJob.taskID,
        currentJob.generation,
      );
      if (!lease) {
        throw new Error(
          `Task ${requested} cannot queue a message: message/control lease unavailable`,
        );
      }

      let keepLeaseUntilSettled = false;
      try {
        assertMessageLease(options.backgroundJobBoard, lease, requested);
        getCurrentTaskMessageJob(
          options.backgroundJobBoard,
          parentSessionID,
          requested,
          lease.taskID,
          lease.generation,
        );

        const session = getClient(options.input).session;
        const prompt = session.prompt.bind(session);
        const transportJob = getCurrentTaskMessageJob(
          options.backgroundJobBoard,
          parentSessionID,
          requested,
          lease.taskID,
          lease.generation,
        );
        const response = await awaitMessageTransport(
          options.backgroundJobBoard,
          lease,
          () =>
            prompt({
              path: { id: lease.taskID },
              body: {
                agent: transportJob.agent,
                noReply: true,
                parts: [{ type: 'text', text: args.message.trim() }],
              },
              throwOnError: true,
            }),
          options.messageTimeoutMs ?? DEFAULT_MESSAGE_TIMEOUT_MS,
        );
        assertMessageLease(options.backgroundJobBoard, lease, requested);
        assertSuccessfulMessageResponse(response);

        const latestJob = getCurrentTaskMessageJob(
          options.backgroundJobBoard,
          parentSessionID,
          requested,
          lease.taskID,
          lease.generation,
        );
        return `Message queued for ${latestJob.alias} (${latestJob.taskID}) without launching or resuming it.`;
      } catch (error) {
        keepLeaseUntilSettled =
          error instanceof MessageLeaseOperationTimeoutError && error.pending;
        throw error;
      } finally {
        if (!keepLeaseUntilSettled) {
          options.backgroundJobBoard.releaseLease(lease);
        }
      }
    },
  });

  return { task_message };
}

function assertMessageLease(
  backgroundJobBoard: BackgroundJobStore,
  lease: NonNullable<ReturnType<BackgroundJobStore['acquireMessageLease']>>,
  requested: string,
): void {
  if (lease.kind !== 'message' || !backgroundJobBoard.validateLease(lease)) {
    throw new Error(
      `Task ${requested} message lease is no longer valid; refusing stale message`,
    );
  }
}

async function awaitMessageTransport<T>(
  backgroundJobBoard: BackgroundJobStore,
  lease: NonNullable<ReturnType<BackgroundJobStore['acquireMessageLease']>>,
  operation: () => Promise<T>,
  timeoutMs: number,
): Promise<T> {
  let timedOut = false;
  let settled = false;
  const underlying = Promise.resolve().then(operation);
  const tracked = underlying.then(
    (value) => {
      settled = true;
      if (timedOut) backgroundJobBoard.releaseLease(lease);
      return value;
    },
    (error: unknown) => {
      settled = true;
      if (timedOut) backgroundJobBoard.releaseLease(lease);
      throw error;
    },
  );

  try {
    return await withTimeout(
      tracked,
      timeoutMs,
      `Task message transport timed out after ${timeoutMs}ms`,
    );
  } catch (error) {
    if (!(error instanceof OperationTimeoutError)) throw error;
    timedOut = true;
    const pending = !settled;
    if (!pending) backgroundJobBoard.releaseLease(lease);
    throw new MessageLeaseOperationTimeoutError(error.message, pending);
  }
}

function assertSuccessfulMessageResponse(response: unknown): void {
  if (!isRecord(response) || response.error === undefined) return;
  if (response.error === null) return;
  throw new Error(
    `Task message transport failed: ${errorText(response.error)}`,
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

function getCurrentTaskMessageJob(
  backgroundJobBoard: BackgroundJobStore,
  parentSessionID: string,
  requested: string,
  expectedTaskID: string,
  expectedGeneration: number,
): NonNullable<ReturnType<BackgroundJobStore['get']>> {
  const current = backgroundJobBoard.get(expectedTaskID);
  const resolved = backgroundJobBoard.resolve(parentSessionID, requested);
  if (!current || !resolved || resolved.taskID !== expectedTaskID) {
    throw new Error(
      `Task ${requested} is no longer tracked; refusing stale message`,
    );
  }
  if (
    current.taskID !== expectedTaskID ||
    current.generation !== expectedGeneration ||
    resolved.generation !== expectedGeneration
  ) {
    throw new Error(
      `Task ${requested} run generation changed; refusing stale message`,
    );
  }
  if (current.cancellationRequested) {
    throw new Error(
      `Task ${requested} cannot queue a message: cancellation was requested`,
    );
  }
  if (current.state !== 'running') {
    throw new Error(
      `Task ${requested} cannot queue a message: board state is ${current.state}, not running`,
    );
  }
  return current;
}
