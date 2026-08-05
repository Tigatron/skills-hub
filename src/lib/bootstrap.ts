import { api } from './api';

export { CommandError as BootstrapCommandError } from './api';

export async function getBootstrapState() {
  return api.bootstrapGetState();
}
