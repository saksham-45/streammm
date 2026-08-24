import { describe, expect, it } from "vitest";
import {
  analysisFromModel,
  askFromModel,
  parseJsonObject,
  DEFAULT_MODEL,
} from "../src/llm";

describe("llm json contract", () => {
  it("parses fenced model JSON", () => {
    const obj = parseJsonObject("```json\n{\"summary\":\"hud\",\"questions\":[]}\n```");
    expect(obj.summary).toBe("hud");
    expect(obj.questions).toEqual([]);
  });

  it("extracts the first JSON object from surrounding text", () => {
    const obj = parseJsonObject('Sure. {"answer":"jump","confidence":80,"reasoning":"prompt says jump"} trailing');
    expect(obj.answer).toBe("jump");
    expect(obj.confidence).toBe(80);
  });

  it("clamps analysis fields", () => {
    const a = analysisFromModel({
      summary: "boss fight",
      questions: [{ question: "parry?", answer: "yes", confidence: 140, reasoning: "flash" }],
    });
    expect(a.summary).toBe("boss fight");
    expect(a.questions[0].confidence).toBe(100);
    expect(a.questions[0].answer).toBe("yes");
  });

  it("clamps ask confidence and defaults missing fields", () => {
    const a = askFromModel({ answer: "left", confidence: -12 });
    expect(a.answer).toBe("left");
    expect(a.confidence).toBe(0);
    expect(a.reasoning).toBe("");
  });

  it("uses the DeepSeek vision model id", () => {
    expect(DEFAULT_MODEL).toBe("deepseek-v4-flash-vision-exp");
  });
});
