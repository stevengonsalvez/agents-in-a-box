const fs = require('fs');
const path = require('path');
const os = require('os');
const { execSync } = require('child_process');

const TOOL_CONFIG = {
    amazonq: {
        ruleGlob: 'q-rulestore-rule.md',
        ruleDir: 'amazonq',
        targetSubdir: '.amazonq/rules',
        mcpFile: 'amazonq/mcp.json',
        mcpTarget: '.amazonq/mcp.json',
        sharedContentDir: 'claude-code',
        copySharedContent: true,
        excludeFiles: ['CLAUDE.md', 'settings.local.json'],
        specialCopies: [
            {
                source: 'amazonq/AmazonQ.md',
                dest: '.amazonq/rules/AmazonQ.md'
            }
        ],
        linkedFiles: [
            {
                source: 'amazonq/AmazonQ.md',
                linkName: 'AmazonQ.md'
            }
        ],
    },
    cline: {
        ruleGlob: 'cline-rulestore-rule.md',
        ruleDir: 'cline',
        targetSubdir: '.clinerules',
    },
    roo: {
        ruleGlob: 'roo-rulestore-rule.md',
        ruleDir: 'roo',
        targetSubdir: '.roo/rules',
    },
    cursor: {
        ruleGlob: 'cursor-rulestore-rule.md',
        ruleDir: 'cursor',
        targetSubdir: '.cursor/rules',
    },
    claude: {
        ruleGlob: 'cursor-rulestore-rule.md',
        ruleDir: 'cursor',
        targetSubdir: '.claude/rules',
    },
    'claude-code': {
        ruleDir: 'claude-code',
        targetSubdir: '.claude',
        copyEntireFolder: true,
        excludeFiles: ['settings.local.json'],
        templateSubstitutions: {
            'CLAUDE.md': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            }
        }
    },
    gemini: {
        ruleGlob: 'GEMINI.md',
        ruleDir: 'gemini',
        targetSubdir: '.gemini',
        sharedContentDir: 'claude-code',
        copySharedContent: true,
        excludeFiles: ['CLAUDE.md'],
        settingsFile: 'gemini/settings.json',
    },
};

const ALWAYS_COPY_RULES = [
    'rule-interpreter-rule.md',
    'rulestyle-rule.md',
];

describe('CLI Rule Copier', () => {
    const tempDir = path.join(__dirname, 'tmp-test-folder');
    const homeDir = os.homedir();

    afterEach(() => {
        if (fs.existsSync(tempDir)) {
            fs.rmSync(tempDir, { recursive: true, force: true });
        }
    });

    it('installs Spec Kit assets via git clone using --sdd (local fake repo)', () => {
        const fakeRepo = path.join(tempDir, 'fake-spec-kit');
        const mk = (p, content='') => { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, content); };
        mk(path.join(fakeRepo, '.claude/commands/specify.md'), 'specify');
        mk(path.join(fakeRepo, '.claude/commands/plan.md'), 'plan');
        mk(path.join(fakeRepo, '.claude/commands/tasks.md'), 'tasks');
        mk(path.join(fakeRepo, 'templates/plan-template.md'), 'plan-template');
        mk(path.join(fakeRepo, 'templates/spec-template.md'), 'spec-template');
        mk(path.join(fakeRepo, 'templates/tasks-template.md'), 'tasks-template');
        mk(path.join(fakeRepo, 'templates/agent-file-template.md'), 'agent-file');
        mk(path.join(fakeRepo, 'memory/constitution.md'), 'constitution');
        mk(path.join(fakeRepo, 'scripts/create-new-feature.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/setup-plan.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/check-task-prerequisites.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/common.sh'), '');
        mk(path.join(fakeRepo, 'scripts/get-feature-paths.sh'), '');
        mk(path.join(fakeRepo, 'scripts/update-agent-context.sh'), '');
        execSync('git init && git add . && git -c user.name="T" -c user.email="t@e" commit -m init', { cwd: fakeRepo, stdio: 'pipe' });

        const target = path.join(tempDir, 'sdd-project');
        fs.mkdirSync(target, { recursive: true });

        execSync(`node bootstrap.js --sdd --targetFolder=${target}`, { stdio: 'pipe', env: { ...process.env, SPEC_KIT_REPO: fakeRepo } });

        // Commands copied
        expect(fs.existsSync(path.join(target, '.claude', 'commands', 'specify.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, '.claude', 'commands', 'plan.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, '.claude', 'commands', 'tasks.md'))).toBe(true);

        // Scripts copied and executable
        const scripts = [
            'create-new-feature.sh',
            'setup-plan.sh',
            'check-task-prerequisites.sh',
            'common.sh',
            'get-feature-paths.sh',
            'update-agent-context.sh',
        ];
        for (const s of scripts) {
            const p = path.join(target, 'scripts', s);
            expect(fs.existsSync(p)).toBe(true);
            const mode = fs.statSync(p).mode & 0o111;
            expect(mode).toBeGreaterThan(0);
        }

        // Templates
        expect(fs.existsSync(path.join(target, 'templates', 'spec-template.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'templates', 'plan-template.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'templates', 'tasks-template.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'templates', 'agent-file-template.md'))).toBe(true);

        // Memory
        expect(fs.existsSync(path.join(target, 'memory', 'constitution.md'))).toBe(true);
    });

    it('creates .bak when overwriting different SDD template', () => {
        const fakeRepo = path.join(tempDir, 'fake-spec-kit-bak');
        const mk = (p, content='') => { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, content); };
        mk(path.join(fakeRepo, '.claude/commands/specify.md'), 'specify');
        mk(path.join(fakeRepo, '.claude/commands/plan.md'), 'plan');
        mk(path.join(fakeRepo, '.claude/commands/tasks.md'), 'tasks');
        mk(path.join(fakeRepo, 'templates/plan-template.md'), 'plan-template');
        mk(path.join(fakeRepo, 'templates/spec-template.md'), 'spec-template');
        mk(path.join(fakeRepo, 'templates/tasks-template.md'), 'tasks-template');
        mk(path.join(fakeRepo, 'templates/agent-file-template.md'), 'agent-file');
        mk(path.join(fakeRepo, 'memory/constitution.md'), 'constitution');
        mk(path.join(fakeRepo, 'scripts/create-new-feature.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/setup-plan.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/check-task-prerequisites.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/common.sh'), '');
        mk(path.join(fakeRepo, 'scripts/get-feature-paths.sh'), '');
        mk(path.join(fakeRepo, 'scripts/update-agent-context.sh'), '');
        execSync('git init && git add . && git -c user.name="T" -c user.email="t@e" commit -m init', { cwd: fakeRepo, stdio: 'pipe' });

        const target = path.join(tempDir, 'sdd-bak');
        fs.mkdirSync(path.join(target, 'templates'), { recursive: true });
        fs.writeFileSync(path.join(target, 'templates', 'plan-template.md'), 'DIFFERENT');

        execSync(`node bootstrap.js --sdd --targetFolder=${target}`, { stdio: 'pipe', env: { ...process.env, SPEC_KIT_REPO: fakeRepo } });

        expect(fs.existsSync(path.join(target, 'templates', 'plan-template.md.bak'))).toBe(true);
    });

    it('SDD smoke test: git clone → install → git init → /specify → /plan', () => {
        const fakeRepo = path.join(tempDir, 'fake-spec-kit-smoke');
        const mk = (p, content='') => { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, content); };
        mk(path.join(fakeRepo, '.claude/commands/specify.md'), 'specify');
        mk(path.join(fakeRepo, '.claude/commands/plan.md'), 'plan');
        mk(path.join(fakeRepo, '.claude/commands/tasks.md'), 'tasks');
        mk(path.join(fakeRepo, 'scripts/create-new-feature.sh'), `#!/usr/bin/env bash
set -e
REPO_ROOT=$(pwd)
SPECS_DIR="$REPO_ROOT/specs"
mkdir -p "$SPECS_DIR"
BRANCH_NAME="001-test-feature"
git checkout -b "$BRANCH_NAME" >/dev/null 2>&1 || git checkout "$BRANCH_NAME" >/dev/null 2>&1
FEATURE_DIR="$SPECS_DIR/$BRANCH_NAME"
mkdir -p "$FEATURE_DIR"
SPEC_FILE="$FEATURE_DIR/spec.md"
echo X > "$SPEC_FILE"
printf '{"BRANCH_NAME":"%s","SPEC_FILE":"%s"}\n' "$BRANCH_NAME" "$SPEC_FILE"
`);
        mk(path.join(fakeRepo, 'scripts/setup-plan.sh'), `#!/usr/bin/env bash
set -e
REPO_ROOT=$(pwd)
BRANCH=$(git rev-parse --abbrev-ref HEAD)
FEATURE_DIR="$REPO_ROOT/specs/$BRANCH"
mkdir -p "$FEATURE_DIR"
IMPL_PLAN="$FEATURE_DIR/plan.md"
echo PLAN > "$IMPL_PLAN"
printf '{"FEATURE_SPEC":"%s","IMPL_PLAN":"%s","SPECS_DIR":"%s","BRANCH":"%s"}\n' "$FEATURE_DIR/spec.md" "$IMPL_PLAN" "$FEATURE_DIR" "$BRANCH"
`);
        mk(path.join(fakeRepo, 'scripts/check-task-prerequisites.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/common.sh'), '');
        mk(path.join(fakeRepo, 'scripts/get-feature-paths.sh'), '');
        mk(path.join(fakeRepo, 'scripts/update-agent-context.sh'), '');
        mk(path.join(fakeRepo, 'templates/plan-template.md'), 'plan-template');
        mk(path.join(fakeRepo, 'templates/spec-template.md'), 'spec-template');
        mk(path.join(fakeRepo, 'templates/tasks-template.md'), 'tasks-template');
        mk(path.join(fakeRepo, 'templates/agent-file-template.md'), 'agent-file');
        mk(path.join(fakeRepo, 'memory/constitution.md'), 'constitution');
        execSync('git init && git add . && git -c user.name="T" -c user.email="t@e" commit -m init', { cwd: fakeRepo, stdio: 'pipe' });

        const target = path.join(tempDir, 'sdd-smoke');
        fs.mkdirSync(target, { recursive: true });

        execSync(`node bootstrap.js --sdd --targetFolder=${target}`, { stdio: 'pipe', env: { ...process.env, SPEC_KIT_REPO: fakeRepo } });

        // Init git and create initial commit to satisfy HEAD-based scripts
        execSync('git init', { cwd: target, stdio: 'pipe' });
        execSync('git -c user.name="Test" -c user.email="test@example.com" commit --allow-empty -m "init"', { cwd: target, stdio: 'pipe' });
        fs.writeFileSync(path.join(target, '.gitignore'), 'node_modules\n');
        execSync('git add . && git commit -m "init"', { cwd: target, stdio: 'pipe' });

        // Run /specify script
        const out = execSync('bash scripts/create-new-feature.sh --json "Sample SDD feature"', { cwd: target, stdio: 'pipe' }).toString();
        const created = JSON.parse(out);
        expect(created.BRANCH_NAME).toMatch(/^[0-9]{3}-/);
        expect(fs.existsSync(created.SPEC_FILE)).toBe(true);
        expect(created.SPEC_FILE).toContain(path.join(target, 'specs'));

        // Run /plan setup
        const out2 = execSync('bash scripts/setup-plan.sh --json', { cwd: target, stdio: 'pipe' }).toString();
        const setup = JSON.parse(out2);
        expect(setup.IMPL_PLAN).toContain(path.join(target, 'specs'));
        expect(fs.existsSync(setup.IMPL_PLAN)).toBe(true);
    });

    it('always copies the tool rule and default rules in non-interactive mode', () => {
        const tool = 'amazonq';
        const config = TOOL_CONFIG[tool];
        const target = path.join(tempDir, tool);
        fs.mkdirSync(target, { recursive: true });

        const command = `node bootstrap.js --tool=${tool} --targetFolder=${target}`;
        execSync(command, {
            stdio: 'pipe',
            env: { ...process.env },
        });

        const destDir = path.join(target, config.targetSubdir);
        expect(fs.existsSync(path.join(destDir, config.ruleGlob))).toBe(true);
        for (const rule of ALWAYS_COPY_RULES) {
            expect(fs.existsSync(path.join(destDir, rule))).toBe(true);
        }
    });

    it('copies shared content to gemini project folder with correct structure', () => {
        const tool = 'gemini';
        const config = TOOL_CONFIG[tool];
        const target = path.join(tempDir, tool);
        fs.mkdirSync(target, { recursive: true });

        const command = `node bootstrap.js --tool=${tool} --targetFolder=${target}`;
        execSync(command, {
            stdio: 'pipe',
            env: { ...process.env },
        });

        const destDir = path.join(target, config.targetSubdir);
        expect(fs.existsSync(destDir)).toBe(true);
        expect(fs.existsSync(path.join(destDir, 'GEMINI.md'))).toBe(true);
        expect(fs.existsSync(path.join(destDir, 'commands'))).toBe(true);
        expect(fs.existsSync(path.join(destDir, 'templates'))).toBe(true);
        expect(fs.existsSync(path.join(destDir, 'session', 'current-session.yaml'))).toBe(true);
        // Should NOT have CLAUDE.md
        expect(fs.existsSync(path.join(destDir, 'CLAUDE.md'))).toBe(false);

        // Check commands folder has files
        const commandsDir = path.join(destDir, 'commands');
        const commandFiles = fs.readdirSync(commandsDir);
        expect(commandFiles.length).toBeGreaterThan(0);
        expect(commandFiles.some(f => f.endsWith('.md'))).toBe(true);

        // Check template substitution
        const geminiContent = fs.readFileSync(path.join(destDir, 'GEMINI.md'), 'utf8');
        expect(geminiContent).toContain('.gemini/session/current-session.yaml');
        expect(geminiContent).not.toContain('{{TOOL_DIR}}');
        
        // Check settings.json is copied to .gemini folder
        expect(fs.existsSync(path.join(destDir, 'settings.json'))).toBe(true);
        const settingsContent = fs.readFileSync(path.join(destDir, 'settings.json'), 'utf8');
        const settings = JSON.parse(settingsContent);
        expect(settings.theme).toBe('GitHub');
        expect(settings.mcpServers).toBeDefined();
    });

    it('copies amazonq files with correct structure', () => {
        const tool = 'amazonq';
        const config = TOOL_CONFIG[tool];
        const target = path.join(tempDir, tool);
        fs.mkdirSync(target, { recursive: true });

        const command = `node bootstrap.js --tool=${tool} --targetFolder=${target}`;
        execSync(command, {
            stdio: 'pipe',
            env: { ...process.env },
        });

        // Check rules directory
        const rulesDir = path.join(target, config.targetSubdir);
        expect(fs.existsSync(path.join(rulesDir, config.ruleGlob))).toBe(true);
        for (const rule of ALWAYS_COPY_RULES) {
            expect(fs.existsSync(path.join(rulesDir, rule))).toBe(true);
        }

        // Check .amazonq/rules directory structure (shared content is in rules dir)
        expect(fs.existsSync(path.join(rulesDir, 'commands'))).toBe(true);
        expect(fs.existsSync(path.join(rulesDir, 'templates'))).toBe(true);
        expect(fs.existsSync(path.join(rulesDir, 'session', 'current-session.yaml'))).toBe(true);

        // Check commands folder has files
        const commandsDir = path.join(rulesDir, 'commands');
        const commandFiles = fs.readdirSync(commandsDir);
        expect(commandFiles.length).toBeGreaterThan(0);
        expect(commandFiles.some(f => f.endsWith('.md'))).toBe(true);

        // Check that settings.local.json is excluded
        expect(fs.existsSync(path.join(rulesDir, 'settings.local.json'))).toBe(false);

        // Check AmazonQ.md in the rules directory
        expect(fs.existsSync(path.join(rulesDir, 'AmazonQ.md'))).toBe(true);

        // Check for the linked AmazonQ.md in the project root
        const rootAmazonQPath = path.join(target, 'AmazonQ.md');
        expect(fs.existsSync(rootAmazonQPath)).toBe(true);
        const rootAmazonQContent = fs.readFileSync(rootAmazonQPath, 'utf8');
        expect(rootAmazonQContent).toBe('@.amazonq/rules/AmazonQ.md');

        // Check mcp.json is copied to .amazonq folder
        expect(fs.existsSync(path.join(target, '.amazonq', 'mcp.json'))).toBe(true);
        const mcpContent = fs.readFileSync(path.join(target, '.amazonq', 'mcp.json'), 'utf8');
        const mcpConfig = JSON.parse(mcpContent);
        expect(mcpConfig.mcpServers).toBeDefined();
        expect(mcpConfig.mcpServers['container-use']).toBeDefined();

        // Check template substitution
        const amazonqContent = fs.readFileSync(path.join(rulesDir, 'AmazonQ.md'), 'utf8');
        expect(amazonqContent).toContain('.amazonq/session/current-session.yaml');
        expect(amazonqContent).not.toContain('{{TOOL_DIR}}');
    });

    it('claude-code copies to home directory with correct paths', () => {
        const tool = 'claude-code';
        const config = TOOL_CONFIG[tool];
        const mockHomeDir = path.join(tempDir, 'mock-home');
        fs.mkdirSync(mockHomeDir, { recursive: true });
        const destDir = path.join(mockHomeDir, config.targetSubdir);

        const command = `node bootstrap.js --tool=${tool} --homeDir=${mockHomeDir}`;
        execSync(command, {
            stdio: 'pipe',
            env: { ...process.env },
        });

        expect(fs.existsSync(destDir)).toBe(true);
        expect(fs.existsSync(path.join(destDir, 'CLAUDE.md'))).toBe(true);
        expect(fs.existsSync(path.join(destDir, 'session', 'current-session.yaml'))).toBe(true);

        // Check commands folder exists and has files
        const commandsDir = path.join(destDir, 'commands');
        expect(fs.existsSync(commandsDir)).toBe(true);
        const commandFiles = fs.readdirSync(commandsDir);
        expect(commandFiles.length).toBeGreaterThan(0);
        expect(commandFiles.some(f => f.endsWith('.md'))).toBe(true);

        // Check templates folder exists
        expect(fs.existsSync(path.join(destDir, 'templates'))).toBe(true);

        // Check template substitution
        const claudeContent = fs.readFileSync(path.join(destDir, 'CLAUDE.md'), 'utf8');
        expect(claudeContent).toContain('.claude/session/current-session.yaml');
        expect(claudeContent).toContain('~/.claude/docs/');
        expect(claudeContent).not.toContain('{{TOOL_DIR}}');
        expect(claudeContent).not.toContain('{{HOME_TOOL_DIR}}');

        // Check that settings.local.json is excluded
        expect(fs.existsSync(path.join(destDir, 'settings.local.json'))).toBe(false);
    });

    it('only copies agents folder for claude-code tool, not for other tools', () => {
        // Test that claude-code DOES copy agents folder
        const claudeCodeTool = 'claude-code';
        const claudeCodeConfig = TOOL_CONFIG[claudeCodeTool];
        const claudeCodeMockHomeDir = path.join(tempDir, 'claude-code-home');
        fs.mkdirSync(claudeCodeMockHomeDir, { recursive: true });
        const claudeCodeDestDir = path.join(claudeCodeMockHomeDir, claudeCodeConfig.targetSubdir);

        const claudeCodeCommand = `node bootstrap.js --tool=${claudeCodeTool} --homeDir=${claudeCodeMockHomeDir}`;
        execSync(claudeCodeCommand, {
            stdio: 'pipe',
            env: { ...process.env },
        });

        // claude-code SHOULD have agents folder
        expect(fs.existsSync(path.join(claudeCodeDestDir, 'agents'))).toBe(true);
        // Check that agentmaker.md exists in agents folder
        expect(fs.existsSync(path.join(claudeCodeDestDir, 'agents', 'meta', 'agentmaker.md'))).toBe(true);

        // Test that gemini tool does NOT copy agents folder
        const geminiTool = 'gemini';
        const geminiConfig = TOOL_CONFIG[geminiTool];
        const geminiTarget = path.join(tempDir, 'gemini-test');
        fs.mkdirSync(geminiTarget, { recursive: true });

        const geminiCommand = `node bootstrap.js --tool=${geminiTool} --targetFolder=${geminiTarget}`;
        execSync(geminiCommand, {
            stdio: 'pipe',
            env: { ...process.env },
        });

        const geminiDestDir = path.join(geminiTarget, geminiConfig.targetSubdir);
        // gemini should NOT have agents folder
        expect(fs.existsSync(path.join(geminiDestDir, 'agents'))).toBe(false);

        // Test that amazonq tool does NOT copy agents folder
        const amazonqTool = 'amazonq';
        const amazonqConfig = TOOL_CONFIG[amazonqTool];
        const amazonqTarget = path.join(tempDir, 'amazonq-test');
        fs.mkdirSync(amazonqTarget, { recursive: true });

        const amazonqCommand = `node bootstrap.js --tool=${amazonqTool} --targetFolder=${amazonqTarget}`;
        execSync(amazonqCommand, {
            stdio: 'pipe',
            env: { ...process.env },
        });

        const amazonqDestDir = path.join(amazonqTarget, amazonqConfig.targetSubdir);
        // amazonq should NOT have agents folder  
        expect(fs.existsSync(path.join(amazonqDestDir, 'agents'))).toBe(false);
    });
});

// --- SDD (Spec-Driven Development) tests ---
describe('Spec-Driven Development (SDD) Setup', () => {
    const tempDir = path.join(__dirname, 'tmp-test-folder-sdd');
    const specKitRoot = process.env.SPEC_KIT_PATH || '/Users/stevengonsalvez/d/git/spec-kit';

    beforeEach(() => {
        if (fs.existsSync(tempDir)) {
            fs.rmSync(tempDir, { recursive: true, force: true });
        }
        fs.mkdirSync(tempDir, { recursive: true });
    });

    afterAll(() => {
        if (fs.existsSync(tempDir)) {
            fs.rmSync(tempDir, { recursive: true, force: true });
        }
    });

    it('copies SDD assets using --sdd into a project folder (local fake repo)', () => {
        // build a minimal local fake repo to avoid network
        const fakeRepo = path.join(tempDir, 'fake-spec-kit-sdd');
        const mk = (p, content='') => { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, content); };
        mk(path.join(fakeRepo, '.claude/commands/specify.md'), 'specify');
        mk(path.join(fakeRepo, '.claude/commands/plan.md'), 'plan');
        mk(path.join(fakeRepo, '.claude/commands/tasks.md'), 'tasks');
        mk(path.join(fakeRepo, 'templates/plan-template.md'), 'plan-template');
        mk(path.join(fakeRepo, 'templates/spec-template.md'), 'spec-template');
        mk(path.join(fakeRepo, 'templates/tasks-template.md'), 'tasks-template');
        mk(path.join(fakeRepo, 'templates/agent-file-template.md'), 'agent-file');
        mk(path.join(fakeRepo, 'memory/constitution.md'), 'constitution');
        mk(path.join(fakeRepo, 'scripts/create-new-feature.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/setup-plan.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/check-task-prerequisites.sh'), '#!/usr/bin/env bash\necho {}');
        mk(path.join(fakeRepo, 'scripts/common.sh'), '');
        mk(path.join(fakeRepo, 'scripts/get-feature-paths.sh'), '');
        mk(path.join(fakeRepo, 'scripts/update-agent-context.sh'), '');
        execSync('git init && git add . && git -c user.name="T" -c user.email="t@e" commit -m init', { cwd: fakeRepo, stdio: 'pipe' });

        const target = path.join(tempDir, 'sdd-project');
        fs.mkdirSync(target, { recursive: true });

        const cmd = `node bootstrap.js --sdd --targetFolder=${target}`;
        execSync(cmd, { cwd: path.join(__dirname), stdio: 'pipe', env: { ...process.env, SPEC_KIT_REPO: fakeRepo } });

        // Verify core folders
        expect(fs.existsSync(path.join(target, '.claude', 'commands'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'scripts'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'templates'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'memory'))).toBe(true);

        // Verify key files
        expect(fs.existsSync(path.join(target, '.claude/commands/specify.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, '.claude/commands/plan.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, '.claude/commands/tasks.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'templates/spec-template.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'templates/plan-template.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'templates/tasks-template.md'))).toBe(true);
        expect(fs.existsSync(path.join(target, 'memory/constitution.md'))).toBe(true);

        // Script executability
        const script = path.join(target, 'scripts/create-new-feature.sh');
        const st = fs.statSync(script);
        expect(st.mode & 0o111).not.toBe(0);
    });

    // Full smoke coverage exists in the main suite; duplicated flow removed here to avoid redundancy.
});
