// The declarations of `@microsoft/fabric-user-data-functions` 2.0.0 that metadata generation
// resolves: `RayfinContext`'s type parameters, the `AudienceType` string enum, the `WithSql`
// marker, and `func`.

export declare enum AudienceType {
  Sql = 'Sql',
  Storage = 'Storage',
  Fabric = 'Fabric',
  AzureAI = 'AzureAI',
  ADO = 'ADO',
}

export declare class FabricItem {
  readonly endpoints?: Record<string, unknown>;
}

export declare class RayfinContext<
  TSchema extends Record<string, any> = Record<string, any>,
  TokenTypes extends AudienceType = never,
  TSecretNames extends string = string,
> {
  readonly Tokens: { readonly [K in TokenTypes]: string };
  readonly Secrets: { readonly [K in TSecretNames]: string };
  readonly schema?: TSchema;
}

export interface WithSql {
  readonly sql: unknown;
}

export declare class UserDataFunctions {
  constructor(srcRoot?: string, metadataPath?: string);
  func(name: string, fn: (...args: any[]) => any, connections?: readonly unknown[]): void;
}
