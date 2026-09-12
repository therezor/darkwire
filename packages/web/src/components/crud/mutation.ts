/**
 * The shape every CRUD screen's writes are handed back in.
 *
 * It started in `use-automation.ts` and stayed there while automation was the
 * only feature with a hook module. Sessions is the second, and a type describing
 * "how this app returns a mutation" reached by importing one *feature* from
 * another is a dependency that says nothing true — sessions do not depend on
 * automation. So it moved to where the rest of the list vocabulary already
 * lives.
 *
 * `onSuccess` receives what the server returned, not nothing. The create flows
 * navigate into the editor for the record they just made, and the id is assigned
 * server-side — without the result there is nothing to navigate to.
 *
 * `pending` rather than `isPending`, and `mutate` rather than the whole
 * TanStack object: what a screen needs is a way to start the write and a flag to
 * disable a button with. Handing back the mutation itself would let a component
 * reach for `reset`, `failureCount` and `submittedAt`, and the point of a hook
 * module is that the component does not know what library is underneath it.
 */
export interface MutationHandle<T, R = void> {
  readonly mutate: (
    input: T,
    options?: { readonly onSuccess?: (result: R) => void },
  ) => void;
  readonly pending: boolean;
}
