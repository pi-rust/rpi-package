import { test, expect } from "@playwright/test";
import { toolSummary } from "../src/ToolDisclosure";

const states = ["pending", "running", "done", "error"] as const;

test("skill name detection uses explicit read aliases and preserves the parent name", () => {
  for (const alias of ["read", "read_file", "readfile", "cat"]) {
    for (const tool of [
      alias,
      `functions.${alias}`,
      ` functions.${alias.toUpperCase()} `,
    ]) {
      for (const key of ["path", "filePath", "file_path"]) {
        for (const [path, skillName] of [
          ["skills/router/SKILL.md", "router"],
          [
            "/workspace/skills/rpi-architecture-boundaries/SKILL.md",
            "rpi-architecture-boundaries",
          ],
          ["C:\\skills\\Ui-Review\\SKILL.md", "Ui-Review"],
          [".\\skills\\工具-规范\\SKILL.md", "工具-规范"],
        ]) {
          expect(toolSummary(tool, { [key]: path }, "done", "zh")).toEqual({
            skillName,
            action: skillName,
            target: path,
          });
        }
      }
    }
  }
});

test("skill name detection does not infer names from commands, contents or directories", () => {
  for (const name of [
    "bash",
    "powershell",
    "write",
    "edit",
    "skills",
    "unknown",
    "reader",
    "other.read",
    "functions.functions.read",
  ]) {
    expect(
      toolSummary(name, { path: "skills/router/SKILL.md" }, "done", "zh"),
    ).not.toHaveProperty("skillName");
  }
  for (const args of [
    undefined,
    null,
    true,
    42,
    "skills/router/SKILL.md",
    [],
    { directory: "skills/router/SKILL.md" },
    { content: "skills/router/SKILL.md" },
    { path: null, filePath: "skills/router/SKILL.md" },
    { path: "", file_path: "skills/router/SKILL.md" },
    { path: ["skills/router/SKILL.md"] },
    { path: { filePath: "skills/router/SKILL.md" } },
    ...[
      "SKILL.md",
      "/SKILL.md",
      "./SKILL.md",
      "../SKILL.md",
      "C:\\SKILL.md",
      "skills/router/skill.md",
      "skills/router/Skill.md",
      "skills/router/SKILL.MD",
      "skills/router/SKILL.md.bak",
      "skills/router/NOTSKILL.md",
      "skills/router/SKILL.md/",
      "skills/router/SKILL.md\n",
      "skills/router/SKILL.md ",
      "skills//SKILL.md",
      "skills/ /SKILL.md",
      "skills/bad\nname/SKILL.md",
      "skills/router/SKILL.md/other",
    ].map((path) => ({ path })),
  ]) {
    expect(toolSummary("read", args, "done", "zh")).not.toHaveProperty(
      "skillName",
    );
  }
});

test("skill summaries keep the same original name across languages and execution states", () => {
  const path = "C:\\skills\\Router-原名\\SKILL.md";
  for (const language of ["en", "zh"] as const) {
    for (const status of states) {
      expect(
        toolSummary(
          "functions.read",
          { path, offset: 4, limit: 10 },
          status,
          language,
        ),
      ).toEqual({
        skillName: "Router-原名",
        action: "Router-原名",
        target: path,
      });
    }
  }
});

test("skill summaries do not rename ordinary reads or change their status mapping", () => {
  for (const language of ["en", "zh"] as const) {
    for (const status of states) {
      const active = status === "pending" || status === "running";
      expect(
        toolSummary("read", { path: "src/readme.md" }, status, language),
      ).toEqual({
        action:
          language === "en"
            ? active
              ? "Reading"
              : "Read"
            : active
              ? "正在读取"
              : "已读取",
        target: "src/readme.md",
      });
    }
  }
});
