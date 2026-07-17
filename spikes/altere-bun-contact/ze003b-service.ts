import { existsSync } from "node:fs";
import { readFile, rename, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

type RotationReceipt = {
	opened: string | null;
};

type RotationEngine = {
	rotationNextReceipt(): RotationReceipt;
};

type RotationEngineConstructor = new (root: string) => RotationEngine;

type RotationCheckpoint = {
	version: 1;
	currentPath: string | null;
};

type RotationRequest = {
	id: number;
	method: "rotation.next";
};

const irPath = process.env.ZE003_IR_TS;
const collectionRoot = process.argv[2];
const checkpointPath = process.argv[3];

if (!irPath) throw new Error("ZE003_IR_TS is required");
if (!collectionRoot) throw new Error("Collection root is required");
if (!checkpointPath) throw new Error("Checkpoint path is required");

const module = (await import(pathToFileURL(irPath).href)) as {
	TypeScriptIntermittentReading: RotationEngineConstructor;
};
const engine = new module.TypeScriptIntermittentReading(collectionRoot);

if (existsSync(checkpointPath)) {
	const checkpoint = JSON.parse(await readFile(checkpointPath, "utf8")) as RotationCheckpoint;
	if (checkpoint.version !== 1) throw new Error("Unsupported rotation checkpoint version");
	if (checkpoint.currentPath) {
		const restored = engine.rotationNextReceipt();
		if (restored.opened !== checkpoint.currentPath) {
			throw new Error(
				`Stale rotation checkpoint: expected ${checkpoint.currentPath}, restored ${restored.opened}`,
			);
		}
	}
}

async function persistCheckpoint(currentPath: string | null): Promise<void> {
	const temporaryPath = `${checkpointPath}.${process.pid}.tmp`;
	const checkpoint: RotationCheckpoint = { version: 1, currentPath };
	await writeFile(temporaryPath, `${JSON.stringify(checkpoint)}\n`);
	await rename(temporaryPath, checkpointPath);
}

const lines = createInterface({ input: process.stdin });
for await (const line of lines) {
	const request = JSON.parse(line) as RotationRequest;
	if (request.method !== "rotation.next") throw new Error(`Unknown method: ${request.method}`);

	const result = engine.rotationNextReceipt();
	await persistCheckpoint(result.opened);
	process.stdout.write(`${JSON.stringify({ id: request.id, result })}\n`);
}
