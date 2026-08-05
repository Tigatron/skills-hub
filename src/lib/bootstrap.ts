import { commands, type AppErrorView, type BootstrapState } from '../bindings';

export class BootstrapCommandError extends Error {
  readonly details: AppErrorView;

  constructor(details: AppErrorView) {
    super(details.message);
    this.name = 'BootstrapCommandError';
    this.details = details;
  }
}

export async function getBootstrapState(): Promise<BootstrapState> {
  const result = await commands.bootstrapGetState();

  if (result.status === 'error') {
    throw new BootstrapCommandError(result.error);
  }

  return result.data;
}
