import { pathToFileURL } from "node:url";

type RotationReceipt = {
	opened: string | null;
};

type RotationEngine = {
	rotationNextReceipt(): RotationReceipt;
};

type RotationEngineConstructor = new (root: string) => RotationEngine;

const irPath = process.env.ZE003_IR_TS;
const collectionRoot = process.argv[2];

if (!irPath) throw new Error("ZE003_IR_TS is required");
if (!collectionRoot) throw new Error("Collection root is required");

const module = (await import(pathToFileURL(irPath).href)) as {
	TypeScriptIntermittentReading: RotationEngineConstructor;
};
const engine = new module.TypeScriptIntermittentReading(collectionRoot);

console.log(JSON.stringify(engine.rotationNextReceipt()));
