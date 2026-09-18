/**
 * The task list a session carries.
 *
 * One tool replaces the whole list on each call, so there is no patch format and
 * no per-task id: the model sends what the list should now be, and the last call
 * wins. That is the whole of the write contract, and it is why a task is two
 * fields.
 *
 * It lives here rather than beside the tool for the reason `subagent.ts` does:
 * six layers read it and none of them can reach the others. The tool writes it,
 * the store holds it, the prompt renders it, the terminal draws it, a chat
 * command prints it and the browser fetches it over REST.
 *
 * The list is in the session's metadata bag rather than a column, the way
 * lineage is. It costs no schema, no index and no query surface, and nothing
 * searches by it. A subagent opens its own session, so keying by session is what
 * gives a delegated run its own list for free.
 *
 * The browser only ever *reads* a list, over REST, so this is the shapes and
 * nothing else — there is no `tasksOf` here to mirror the server's, because
 * nothing in the browser holds a raw metadata bag.
 */

import { z } from 'zod';

/** Where a session records its task list. */
export const TASKS_METADATA_KEY = 'tasks';

/**
 * How many tasks a list may hold.
 *
 * Ten, because the list is re-read on every iteration of every turn and a plan
 * nobody can hold in their head is not a plan. Work that needs more of them
 * needs a smaller first task.
 */
export const MAX_TASKS = 10;

/** How long one task's text may be. */
export const MAX_TASK_CHARS = 100;

/** Where one task has got to. At most one task may be `doing`. */
export const TaskStatusSchema = z.enum(['todo', 'doing', 'done']);
export type TaskStatus = z.infer<typeof TaskStatusSchema>;

/** One line of the plan. */
export const TaskItemSchema = z.object({
  text: z.string().min(1).max(MAX_TASK_CHARS),
  status: TaskStatusSchema,
});
export type TaskItem = z.infer<typeof TaskItemSchema>;
