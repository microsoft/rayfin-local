// Generates `runtimemetadata.json` for a Rayfin functions package, as `rayfin dev` (Rayfin CLI
// 2.0.0) does in its type generation step.
//
// `@microsoft/fabric-user-data-functions` 2.0.0 never parses source at runtime: `new
// UserDataFunctions()` reads `<cwd>/runtimemetadata.json` (schema 3.0), which lists every
// `udf.func(...)` registration with its delegate parameters (a context parameter's type is just
// `RayfinContext`), the audiences declared on its `RayfinContext<Schema, Audiences>` annotation
// and the capabilities intersected with it (`& WithSql` is `sql`). Type arguments are erased by
// the compiler, so these can only come from a build-time pass with the TypeScript type checker.
//
// The analysis follows the CLI's rules exactly, so the output is byte-for-byte what the CLI writes
// for the same source:
// - A TypeScript program from the package's `tsconfig.json`, created with the package's own
//   `typescript`. Without a usable tsconfig, each `src/**/*.ts` file is parsed on its own and the
//   audiences and capabilities are read from the annotation syntax.
// - Files under `src/`, except declaration files, `types.ts` and `*.test.ts` / `*.spec.ts`, in
//   order of their path relative to the package.
// - Calls `<expr>.func('<name>', <arrow function or function expression>, ...)`.
//
// Protocol (`rl_node::run_helper`): `node udf-metadata.mjs --input <in.json> --output <out.json>`.
// Input: `{ functionsDir, resolvePaths?: string[], outputPath?: string }`.
//   `resolvePaths` are extra directories to resolve `typescript` from, after the functions dir and
//   its ancestors; `outputPath`, when set, is written (atomically, only if its content changes).
// Result: `{ json, metadata, written, diagnostics, typescript: { version, path }, typeChecked,
//   sourceFiles }`, where `json` is the exact file content.

import fs from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';

const SCHEMA_VERSION = '3.0';
const CONTEXT_TYPE_NAME = 'RayfinContext';
const UDF_PACKAGE_NAME = '@microsoft/fabric-user-data-functions';
const WITH_SQL_MARKER_NAME = 'WithSql';
const SQL_CAPABILITY = 'sql';
const FABRIC_PARAMETER_TYPES = new Set([
  'FabricContext',
  'FabricSqlConnection',
  'FabricConnection',
  'DataConnection',
  'FabricUdf',
  'FabricLogger',
]);

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--input') args.input = argv[++i];
    else if (argv[i] === '--output') args.output = argv[++i];
  }
  if (!args.input || !args.output) {
    throw new Error('usage: node udf-metadata.mjs --input <file> --output <file>');
  }
  return args;
}

/** The functions package's own compiler, else the closest one up the tree, else the CLI's. */
function loadTypeScript(functionsDir, resolvePaths) {
  const bases = [functionsDir, ...resolvePaths, process.cwd()];
  for (const base of bases) {
    try {
      const require = createRequire(path.join(path.resolve(base), 'package.json'));
      const resolved = require.resolve('typescript');
      return { ts: require(resolved), path: resolved };
    } catch {
      // Not installed here; try the next base.
    }
  }
  for (const base of bases) {
    try {
      const require = createRequire(path.join(path.resolve(base), 'package.json'));
      const cliRequire = createRequire(require.resolve('@microsoft/rayfin-cli/package.json'));
      const resolved = cliRequire.resolve('typescript');
      return { ts: cliRequire(resolved), path: resolved };
    } catch {
      // No Rayfin CLI here either.
    }
  }
  throw new Error(
    `TypeScript was not found for the functions package in ${functionsDir}. ` +
      `Run \`npm install\` in ${functionsDir} (its package.json should list "typescript" in devDependencies).`,
  );
}

/** A program from `<functionsDir>/tsconfig.json`, or null when there is no usable config. */
function createProgram(ts, functionsDir) {
  const tsconfigPath = path.join(functionsDir, 'tsconfig.json');
  if (!fs.existsSync(tsconfigPath)) return null;
  const configFile = ts.readConfigFile(tsconfigPath, ts.sys.readFile);
  if (configFile.error) return null;
  const parsed = ts.parseJsonConfigFileContent(configFile.config, ts.sys, functionsDir);
  if (parsed.errors.length > 0 || parsed.fileNames.length === 0) return null;
  const program = ts.createProgram(parsed.fileNames, parsed.options);
  return { program, checker: program.getTypeChecker() };
}

function isExcludedSourceName(baseName) {
  return baseName === 'types.ts' || /\.(test|spec)\.ts$/.test(baseName);
}

/** `src/**\/*.ts` without dot entries, declaration files, `types.ts` and tests. */
function listSourceFiles(dir, files = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name.startsWith('.')) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      listSourceFiles(full, files);
    } else if (
      entry.isFile() &&
      entry.name.endsWith('.ts') &&
      !entry.name.endsWith('.d.ts') &&
      !isExcludedSourceName(entry.name)
    ) {
      files.push(full);
    }
  }
  return files;
}

function collectSources(ts, functionsDir, srcDir, project) {
  const sources = [];
  if (project) {
    const normalized = srcDir.replace(/\\/g, '/');
    const prefix = normalized.endsWith('/') ? normalized : `${normalized}/`;
    for (const sourceFile of project.program.getSourceFiles()) {
      if (sourceFile.isDeclarationFile) continue;
      const fileName = sourceFile.fileName.replace(/\\/g, '/');
      if (!fileName.startsWith(prefix)) continue;
      if (isExcludedSourceName(fileName.slice(fileName.lastIndexOf('/') + 1))) continue;
      sources.push({ sourceFile, scriptFile: path.relative(functionsDir, sourceFile.fileName) });
    }
  } else {
    for (const file of listSourceFiles(srcDir)) {
      const sourceFile = ts.createSourceFile(
        file,
        fs.readFileSync(file, 'utf8'),
        ts.ScriptTarget.Latest,
        true,
        ts.ScriptKind.TS,
      );
      sources.push({ sourceFile, scriptFile: path.relative(functionsDir, file) });
    }
  }
  sources.sort((a, b) => a.scriptFile.localeCompare(b.scriptFile));
  return sources;
}

function isContextTypeNode(ts, node) {
  if (!ts.isTypeReferenceNode(node)) return false;
  const { typeName } = node;
  return (ts.isQualifiedName(typeName) ? typeName.right.text : typeName.text) === CONTEXT_TYPE_NAME;
}

/**
 * The worker's `rayfinContextMember`: the `RayfinContext` member of an annotation, checked per
 * top-level `&` member so that `RayfinContext<S> & WithSql` counts.
 */
function contextMemberText(typeText) {
  if (!typeText) return undefined;
  const members = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < typeText.length; i += 1) {
    const ch = typeText[i];
    if (ch === '<' || ch === '(' || ch === '[' || ch === '{') depth += 1;
    else if ((ch === '>' && typeText[i - 1] !== '=') || ch === ')' || ch === ']' || ch === '}')
      depth -= 1;
    else if (ch === '&' && depth === 0) {
      members.push(typeText.slice(start, i));
      start = i + 1;
    }
  }
  members.push(typeText.slice(start));
  return members
    .map((member) => member.trim())
    .find((member) => member === CONTEXT_TYPE_NAME || member.startsWith(`${CONTEXT_TYPE_NAME}<`));
}

/** Unwraps the `T | undefined` of an optional parameter; other unions are kept. */
function nonOptionalType(ts, type) {
  if (!type.isUnion()) return type;
  const members = type.types.filter((member) => !(member.flags & ts.TypeFlags.Undefined));
  return members.length === 1 ? members[0] : type;
}

const sdkContextOrigins = new WeakMap();

/** Whether a type is the UDF package's `RayfinContext` class, by its declaring package. */
function isSdkContextType(ts, type) {
  if (
    !(type.flags & ts.TypeFlags.Object) ||
    !(type.objectFlags & (ts.ObjectFlags.Class | ts.ObjectFlags.Reference))
  ) {
    return false;
  }
  const symbol = type.getSymbol();
  if (symbol?.getName() !== CONTEXT_TYPE_NAME) return false;
  const cached = sdkContextOrigins.get(symbol);
  if (cached !== undefined) return cached;
  const isSdkContext =
    symbol.declarations?.some((declaration) => {
      if (!ts.isClassDeclaration(declaration)) return false;
      let directory = path.dirname(declaration.getSourceFile().fileName);
      for (;;) {
        const manifestPath = path.join(directory, 'package.json');
        if (fs.existsSync(manifestPath)) {
          const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
          return manifest?.name === UDF_PACKAGE_NAME;
        }
        const parent = path.dirname(directory);
        if (parent === directory) return false;
        directory = parent;
      }
    }) ?? false;
  sdkContextOrigins.set(symbol, isSdkContext);
  return isSdkContext;
}

/** The CLI's `isRayfinContextParameter`: by annotation text, else through aliases by type. */
function isContextParameter(ts, parameter, sourceFile, checker) {
  if (contextMemberText(parameter.type?.getText(sourceFile)) !== undefined) return true;
  if (!checker || !parameter.type) return false;
  const type = nonOptionalType(ts, checker.getTypeAtLocation(parameter));
  if (type.isUnion()) return false;
  const members = type.isIntersection() ? type.types : [type];
  return members.some((member) => isSdkContextType(ts, member));
}

/** The `RayfinContext` instantiation behind a parameter type, looking through unions. */
function findContextType(type) {
  const candidates = type.isUnionOrIntersection() ? type.types : [type];
  return (
    candidates.find(
      (candidate) =>
        candidate.getSymbol()?.getName() === CONTEXT_TYPE_NAME ||
        candidate.aliasSymbol?.getName() === CONTEXT_TYPE_NAME,
    ) ?? null
  );
}

/** Audience names in a resolved type: string enum members surface as string literals. */
function audienceNamesFromType(ts, type) {
  const names = [];
  const visit = (current) => {
    if (current.flags & ts.TypeFlags.Never) return;
    if (current.isUnion()) {
      current.types.forEach(visit);
      return;
    }
    if (current.isStringLiteral()) {
      if (!names.includes(current.value)) names.push(current.value);
      return;
    }
    const symbolName = current.getSymbol()?.getName();
    if (symbolName && !names.includes(symbolName)) names.push(symbolName);
  };
  visit(type);
  return names;
}

/** Audience names written literally: `AudienceType.Sql`, `Sql` or `'Sql'`. */
function audienceNamesFromTypeNode(ts, node) {
  const names = [];
  const visit = (current) => {
    if (ts.isUnionTypeNode(current)) {
      current.types.forEach(visit);
    } else if (ts.isParenthesizedTypeNode(current)) {
      visit(current.type);
    } else if (
      ts.isLiteralTypeNode(current) &&
      ts.isStringLiteral(current.literal) &&
      current.literal.text.length > 0
    ) {
      if (!names.includes(current.literal.text)) names.push(current.literal.text);
    } else if (ts.isTypeReferenceNode(current)) {
      const { typeName } = current;
      const name = ts.isQualifiedName(typeName) ? typeName.right.text : typeName.text;
      if (name && name !== 'never' && !names.includes(name)) names.push(name);
    }
  };
  visit(node);
  return names;
}

/** A parameter's declared audiences; null when it is not a `RayfinContext`. */
function parameterAudiences(ts, parameter, checker) {
  if (checker) {
    const contextType = findContextType(nonOptionalType(ts, checker.getTypeAtLocation(parameter)));
    if (contextType) {
      const typeArguments = checker.getTypeArguments(contextType);
      return typeArguments.length < 2 ? [] : audienceNamesFromType(ts, typeArguments[1]);
    }
  }
  const typeNode = parameter.type;
  if (!typeNode || !isContextTypeNode(ts, typeNode)) return null;
  const typeArguments = typeNode.typeArguments;
  if (!typeArguments || typeArguments.length < 2) return [];
  return audienceNamesFromTypeNode(ts, typeArguments[1]);
}

/** Audiences across all parameters, deduplicated and sorted (stable across checker and syntax). */
function handlerAudiences(ts, handler, checker) {
  const audiences = [];
  for (const parameter of handler.parameters) {
    for (const audience of parameterAudiences(ts, parameter, checker) ?? []) {
      if (!audiences.includes(audience)) audiences.push(audience);
    }
  }
  return audiences.sort();
}

/** Marker names intersected with a parameter's context: by type, else as written. */
function parameterMarkers(ts, parameter, checker) {
  if (checker) {
    const type = nonOptionalType(ts, checker.getTypeAtLocation(parameter));
    if (type.isIntersection()) {
      const names = [];
      for (const member of type.types) {
        const name = member.aliasSymbol?.getName() ?? member.getSymbol()?.getName();
        if (name && !names.includes(name)) names.push(name);
      }
      if (names.length > 0) return names;
    }
  }
  if (!parameter.type || !ts.isIntersectionTypeNode(parameter.type)) return [];
  const names = [];
  const visit = (current) => {
    if (ts.isParenthesizedTypeNode(current)) {
      visit(current.type);
    } else if (ts.isIntersectionTypeNode(current)) {
      current.types.forEach(visit);
    } else if (ts.isTypeReferenceNode(current)) {
      const { typeName } = current;
      const name = ts.isQualifiedName(typeName) ? typeName.right.text : typeName.text;
      if (name && !names.includes(name)) names.push(name);
    }
  };
  visit(parameter.type);
  return names;
}

/** Capabilities across all parameters, sorted: `WithSql` is `sql`; other markers are not. */
function handlerCapabilities(ts, handler, checker) {
  const capabilities = [];
  for (const parameter of handler.parameters) {
    for (const marker of parameterMarkers(ts, parameter, checker)) {
      if (marker === WITH_SQL_MARKER_NAME && !capabilities.includes(SQL_CAPABILITY)) {
        capabilities.push(SQL_CAPABILITY);
      }
    }
  }
  return capabilities.sort();
}

function isFabricParameter(typeText, name) {
  if (typeText && FABRIC_PARAMETER_TYPES.has(typeText)) return true;
  if (!name) return false;
  const lower = name.toLowerCase();
  return lower === 'context' || lower.includes('connection') || lower.includes('fabric');
}

function analyzeParameters(ts, handler, sourceFile, checker) {
  return handler.parameters.map((parameter, position) => {
    const name = parameter.name.getText(sourceFile);
    const typeText = parameter.type?.getText(sourceFile);
    const fabric = isFabricParameter(typeText, name);
    return {
      name,
      typeText,
      isContext: isContextParameter(ts, parameter, sourceFile, checker),
      optional: parameter.questionToken !== undefined,
      hasDefault: parameter.initializer !== undefined,
      position,
      isFabricParameter: fabric,
      fabricParameterType: fabric ? typeText : undefined,
    };
  });
}

function analyzeFuncCall(ts, node, sourceFile, scriptFile, checker, diagnostics) {
  const callee = node.expression;
  if (!ts.isPropertyAccessExpression(callee) || callee.name.text !== 'func') return null;
  if (node.arguments.length < 2) return null;
  const [nameArgument, handler] = node.arguments;
  if (!ts.isStringLiteral(nameArgument)) return null;
  if (!ts.isArrowFunction(handler) && !ts.isFunctionExpression(handler)) return null;

  const functionName = nameArgument.text;
  const warn = (code, message) => {
    const { line, character } = sourceFile.getLineAndCharacterOfPosition(
      handler.getStart(sourceFile),
    );
    diagnostics.push({
      code,
      severity: 'warning',
      message,
      filePath: scriptFile,
      line: line + 1,
      column: character + 1,
      functionName,
    });
  };
  const contextAudiences = handlerAudiences(ts, handler, checker);
  if (!checker && contextAudiences.length > 0) {
    warn(
      'unverified-context-audience',
      `Function '${functionName}': context audiences [${contextAudiences.join(', ')}] were read ` +
        'from the annotation text because the functions package has no usable tsconfig.json. ' +
        'Add a tsconfig.json, or declare audiences literally, e.g. RayfinContext<Schema, AudienceType.Sql>.',
    );
  }
  const capabilities = handlerCapabilities(ts, handler, checker);
  if (!checker && capabilities.length > 0) {
    warn(
      'unverified-context-capability',
      `Function '${functionName}': capabilities [${capabilities.join(', ')}] were read from the ` +
        'annotation text because the functions package has no usable tsconfig.json. ' +
        'Add a tsconfig.json, or intersect the marker literally, e.g. RayfinContext<Schema> & WithSql.',
    );
  }
  return {
    functionName,
    parameters: analyzeParameters(ts, handler, sourceFile, checker),
    contextAudiences,
    capabilities,
  };
}

function analyzeSourceFile(ts, sourceFile, scriptFile, checker, diagnostics) {
  const functions = [];
  const visit = (node) => {
    if (ts.isCallExpression(node)) {
      const fn = analyzeFuncCall(ts, node, sourceFile, scriptFile, checker, diagnostics);
      if (fn) functions.push(fn);
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);
  return functions;
}

function toRuntimeMetadata(functions) {
  return {
    schemaVersion: SCHEMA_VERSION,
    functions: functions.map((fn) => ({
      functionName: fn.functionName,
      delegateParameters: fn.parameters.map((parameter) => {
        const type = parameter.isContext ? CONTEXT_TYPE_NAME : parameter.typeText;
        // The worker rejects the whole file when `fabricParameterType` and `type` disagree, so a
        // copy of the annotation is replaced with it.
        const fabricParameterType =
          parameter.fabricParameterType !== undefined &&
          parameter.fabricParameterType === parameter.typeText
            ? type
            : parameter.fabricParameterType;
        return {
          name: parameter.name,
          ...(type === undefined ? {} : { type }),
          optional: parameter.optional,
          hasDefault: parameter.hasDefault,
          position: parameter.position,
          isFabricParameter: parameter.isFabricParameter,
          ...(fabricParameterType === undefined ? {} : { fabricParameterType }),
        };
      }),
      contextAudiences: [...fn.contextAudiences],
      capabilities: [...fn.capabilities],
    })),
  };
}

/** Writes `content` to `file` through a sibling temp file, unless it already has that content. */
function writeIfChanged(file, content) {
  try {
    if (fs.readFileSync(file, 'utf8') === content) return false;
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  const temp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temp, content);
  try {
    fs.renameSync(temp, file);
  } catch (error) {
    fs.rmSync(temp, { force: true });
    throw error;
  }
  return true;
}

function generate(input) {
  if (typeof input?.functionsDir !== 'string' || input.functionsDir.length === 0) {
    throw new Error('input.functionsDir is required');
  }
  const functionsDir = path.resolve(input.functionsDir);
  const resolvePaths = Array.isArray(input.resolvePaths) ? input.resolvePaths : [];
  const srcDir = path.join(functionsDir, 'src');
  if (!fs.existsSync(srcDir)) {
    throw new Error(
      `Functions source directory not found: ${srcDir}. Functions are registered with udf.func(...) in src/.`,
    );
  }

  const { ts, path: typescriptPath } = loadTypeScript(functionsDir, resolvePaths);
  const project = createProgram(ts, functionsDir);
  const sources = collectSources(ts, functionsDir, srcDir, project);
  if (sources.length === 0) {
    throw new Error(`No TypeScript source files found in ${srcDir}.`);
  }

  const diagnostics = [];
  const functions = sources.flatMap(({ sourceFile, scriptFile }) =>
    analyzeSourceFile(ts, sourceFile, scriptFile, project?.checker, diagnostics),
  );
  if (functions.length === 0) {
    throw new Error(
      `No udf.func() registrations found in ${srcDir}. ` +
        "Register functions with udf.func('name', async (...) => { ... }).",
    );
  }

  const metadata = toRuntimeMetadata(functions);
  const json = JSON.stringify(metadata, null, 2);
  const written =
    typeof input.outputPath === 'string' && input.outputPath.length > 0
      ? writeIfChanged(path.resolve(input.outputPath), json)
      : null;
  return {
    json,
    metadata,
    written,
    diagnostics,
    typescript: { version: ts.version, path: typescriptPath },
    typeChecked: project !== null,
    sourceFiles: sources.map(({ scriptFile }) => scriptFile.replace(/\\/g, '/')),
  };
}

function errorInfo(error) {
  return error instanceof Error
    ? { message: error.message, stack: error.stack }
    : { message: String(error) };
}

const args = parseArgs(process.argv.slice(2));
let response;
try {
  const input = JSON.parse(fs.readFileSync(args.input, 'utf8'));
  response = { ok: true, result: generate(input) };
} catch (error) {
  response = { ok: false, error: errorInfo(error) };
}
fs.writeFileSync(args.output, JSON.stringify(response));
