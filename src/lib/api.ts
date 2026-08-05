import {
  commands,
  events,
  type ActivityDetail,
  type ActivityItem,
  type ActivityQuery,
  type AnyOperationView,
  type AppErrorView,
  type BatchDeploymentPlanRequest,
  type BatchDeploymentPlanView,
  type BootstrapState,
  type DeploymentHealthView,
  type DeploymentModeDto,
  type DeploymentPage,
  type DeploymentPlanRequest,
  type DeploymentPlanView,
  type DeploymentQuery,
  type DomainInvalidated,
  type ExecuteOperationRequest,
  type InitializeVaultRequest,
  type JobRef,
  type KeepExternalRequest,
  type KeepExternalResult,
  type LibraryPage,
  type LibraryQuery,
  type OperationCancelResult,
  type OperationIdRequest,
  type RegisterTargetRequest,
  type Result,
  type ScanProgress,
  type ScanRequest,
  type ScanRunView,
  type SkillDetail,
  type SkillIdRequest,
  type SkillPreviewRequest,
  type StartupRecoveryReport,
  type TakeoverPlanRequest,
  type TakeoverPlanView,
  type TargetView,
  type TextPreview,
  type TrashEntryView,
  type TrashExecutionView,
  type TrashPlanView,
  type UndeployPlanRequest,
  type VaultStatusView,
  type VaultSummary,
} from '../bindings';

export class CommandError extends Error {
  readonly details: AppErrorView;

  constructor(details: AppErrorView) {
    super(details.message);
    this.name = 'CommandError';
    this.details = details;
  }
}

async function unwrap<T>(result: Promise<Result<T, AppErrorView>>): Promise<T> {
  const value = await result;
  if (value.status === 'error') {
    throw new CommandError(value.error);
  }
  return value.data;
}

export const api = {
  bootstrapGetState: (): Promise<BootstrapState> => unwrap(commands.bootstrapGetState()),
  vaultStatus: (): Promise<VaultStatusView> => unwrap(commands.vaultStatus()),
  vaultInitialize: (request: InitializeVaultRequest): Promise<VaultSummary> =>
    unwrap(commands.vaultInitialize(request)),
  startupRecoveryStatus: (): Promise<StartupRecoveryReport> =>
    unwrap(commands.startupRecoveryStatus()),
  scanStart: (request: ScanRequest = { source: 'universal_global' }): Promise<JobRef> =>
    unwrap(commands.scanStart(request)),
  scanGet: (jobId: string): Promise<ScanRunView> => unwrap(commands.scanGet(jobId)),
  scanCancel: (jobId: string) => unwrap(commands.scanCancel(jobId)),
  libraryList: (query: LibraryQuery): Promise<LibraryPage> => unwrap(commands.libraryList(query)),
  targetRegisterFixture: (request: RegisterTargetRequest): Promise<TargetView> =>
    unwrap(commands.targetRegisterFixture(request)),
  targetsList: (): Promise<TargetView[]> => unwrap(commands.targetsList()),
  deploymentPlan: (request: DeploymentPlanRequest): Promise<DeploymentPlanView> =>
    unwrap(commands.deploymentPlan(request)),
  batchDeploymentPlan: (request: BatchDeploymentPlanRequest): Promise<BatchDeploymentPlanView> =>
    unwrap(commands.batchDeploymentPlan(request)),
  undeployPlan: (request: UndeployPlanRequest): Promise<DeploymentPlanView> =>
    unwrap(commands.undeployPlan(request)),
  deploymentVerify: (deploymentId: string): Promise<DeploymentHealthView> =>
    unwrap(commands.deploymentVerify({ deploymentId })),
  deploymentsList: (query: DeploymentQuery): Promise<DeploymentPage> =>
    unwrap(commands.deploymentsList(query)),
  takeoverKeepExternal: (request: KeepExternalRequest): Promise<KeepExternalResult> =>
    unwrap(commands.takeoverKeepExternal(request)),
  takeoverPlan: (request: TakeoverPlanRequest): Promise<TakeoverPlanView> =>
    unwrap(commands.takeoverPlan(request)),
  operationExecute: (request: ExecuteOperationRequest): Promise<AnyOperationView> =>
    unwrap(commands.operationExecute(request)),
  operationCancel: (request: OperationIdRequest): Promise<OperationCancelResult> =>
    unwrap(commands.operationCancel(request)),
  operationGet: (request: OperationIdRequest): Promise<AnyOperationView> =>
    unwrap(commands.operationGet(request)),
  skillGet: (request: SkillIdRequest): Promise<SkillDetail> => unwrap(commands.skillGet(request)),
  skillPreviewFile: (request: SkillPreviewRequest): Promise<TextPreview> =>
    unwrap(commands.skillPreviewFile(request)),
  activityList: (query: ActivityQuery): Promise<ActivityItem[]> =>
    unwrap(commands.activityList(query)),
  activityDetail: (id: string): Promise<ActivityDetail> => unwrap(commands.activityDetail(id)),
  trashEntriesList: (): Promise<TrashEntryView[]> => unwrap(commands.trashEntriesList()),
  trashMovePlan: (skillId: string): Promise<TrashPlanView> =>
    unwrap(commands.trashMovePlan({ skillId })),
  trashRestorePlan: (entryId: string): Promise<TrashPlanView> =>
    unwrap(commands.trashRestorePlan({ entryId })),
  trashPermanentDeletePlan: (entryId: string, confirmation: string): Promise<TrashPlanView> =>
    unwrap(commands.trashPermanentDeletePlan({ entryId, confirmation })),
  trashExecute: (
    kind: 'move_to_trash' | 'restore' | 'permanently_delete',
    request: { operationId: string; planDigest: string },
  ): Promise<TrashExecutionView> => {
    if (kind === 'move_to_trash') return unwrap(commands.trashMoveExecute(request));
    if (kind === 'restore') return unwrap(commands.trashRestoreExecute(request));
    return unwrap(commands.trashPermanentDeleteExecute(request));
  },
  operationUndoPlan: (request: OperationIdRequest) => unwrap(commands.operationUndoPlan(request)),
};

export type NavId = 'library' | 'deployments' | 'activity';

export type ReviewedPlan =
  | { kind: 'takeover'; plan: TakeoverPlanView }
  | { kind: 'deployment'; plan: DeploymentPlanView }
  | { kind: 'batch'; plan: BatchDeploymentPlanView }
  | {
      kind: 'trash';
      action: 'move_to_trash' | 'restore' | 'permanently_delete';
      plan: TrashPlanView;
    };

export function planIdentity(plan: ReviewedPlan): { operationId: string; planDigest: string } {
  return { operationId: plan.plan.operationId, planDigest: plan.plan.planDigest };
}

export function operationOutcomeLabel(view: AnyOperationView): string {
  return view.value.outcome ?? view.value.state;
}

export function isTerminalOperationState(state: string): boolean {
  const normalized = state.toLowerCase();
  return (
    normalized.includes('final') ||
    normalized.includes('fail') ||
    normalized.includes('cancel') ||
    normalized.includes('rolled')
  );
}

export type ModeChoice = DeploymentModeDto | null;

export async function listenDomainInvalidated(
  onEvent: (payload: DomainInvalidated) => void,
): Promise<() => void> {
  return events.domainInvalidated.listen((event) => {
    onEvent(event.payload);
  });
}

export async function listenScanProgress(
  onEvent: (payload: ScanProgress) => void,
): Promise<() => void> {
  return events.scanProgress.listen((event) => {
    onEvent(event.payload);
  });
}
