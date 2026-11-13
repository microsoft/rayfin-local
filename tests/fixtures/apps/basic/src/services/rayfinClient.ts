import { browserSession } from '@microsoft/rayfin-auth';
import { RayfinClient } from '@microsoft/rayfin-client';
import { dataService } from '@microsoft/rayfin-data';
import {
  fabricSemanticModel,
  type FabricSemanticModel,
} from '@microsoft/rayfin-connector-fabric-semanticmodel';
import { connectorsService, type ConnectorConfig } from '@microsoft/rayfin-connectors';
import { functionsService } from '@microsoft/rayfin-functions';
import { storageService } from '@microsoft/rayfin-storage';

import type { BasicSchema } from '../../rayfin/data/schema';
import type { AppFunctionsSchema } from '../../rayfin/functions/src/types';
import type { Attachments } from '../../rayfin/storage/Attachments';
import type { Avatars } from '../../rayfin/storage/Avatars';

/*
 * `rayfin connector add` would generate the connector schema and config below in
 * rayfin/connectors/salesmodel/schema.ts. They live here because everything under rayfin/
 * (other than functions/) is compiled into crates/rl-model's golden analysis of this fixture.
 */

/** Connectors declared in rayfin/rayfin.yml, keyed by their exact `name`. */
export type AppConnectorsSchema = {
  salesmodel: FabricSemanticModel<'executeQuery'>;
};

const connectorConfigs = {
  salesmodel: { connector: 'fabric-semanticmodel' },
} as const satisfies Record<keyof AppConnectorsSchema, ConnectorConfig>;

/** Storage folders (rayfin/storage), keyed by their normalized `@blob` names. */
export type AppStorageSchema = {
  'todo-attachments': Attachments;
  'user-avatars': Avatars;
};

export interface AppClientConfig {
  baseUrl: string;
  publishableKey: string;
  /** Local-debugging override for the functions route. */
  functionsBaseUrl?: string;
}

export function createAppClient(config: AppClientConfig) {
  return RayfinClient.create({
    baseUrl: config.baseUrl,
    publishableKey: config.publishableKey,
    auth: browserSession({ storage: true }),
    functionsBaseUrl: config.functionsBaseUrl,
    services: {
      data: dataService<BasicSchema>(),
      functions: functionsService<AppFunctionsSchema>(),
      connectors: connectorsService<AppConnectorsSchema>({
        configs: connectorConfigs,
        // Required: the semantic-model runtime decodes the Arrow response.
        runtime: { salesmodel: fabricSemanticModel() },
      }),
      storage: storageService<AppStorageSchema>(),
    },
  });
}

export type AppClient = ReturnType<typeof createAppClient>;
