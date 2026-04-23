#!/usr/bin/env node

import inquirer from 'inquirer';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { readdirSync, statSync } from 'fs';
import yaml from 'js-yaml';
import os from 'os';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const TOOL_CONFIG = {
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
    'claude-code-4.5': {
        ruleDir: 'packages',
        targetSubdir: '.claude',
        usePackagesStructure: true,
        externalDepTypes: ['claude-plugins', 'npx-skills', 'agent-skills'],
        packageMappings: {
            'skills': 'skills',
            'agents': 'agents',
            'utilities/utils': 'utils',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        // Tool-specific files still come from claude-code-4.5/
        toolSpecificFiles: [
            'claude-code-4.5/CLAUDE.md',
            'claude-code-4.5/settings.json',
            'claude-code-4.5/statusline.sh'
        ],
        excludeFiles: ['settings.local.json'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.sh': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.py': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.js': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.ts': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.json': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.yml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.toml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            }
        }
    },
    codex: {
        ruleDir: 'codex',
        targetSubdir: '.codex',
        usePackagesStructure: true,
        forceHomeInstall: true,
        copyClaudeMd: false,
        copySettings: false,
        externalDepTypes: ['npx-skills', 'codex-skills', 'agent-skills'],
        packageMappings: {
            'skills': 'skills',
            'utilities/reflections': 'reflections'
        },
        projectRootCopies: ['AGENTS.md'],
        toolSpecificFiles: ['codex/config.toml'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.sh': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.py': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.js': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.ts': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.json': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.yml': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.toml': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            }
        }
    },
    copilot: {
        ruleDir: 'copilot',
        targetSubdir: '.copilot',
        usePackagesStructure: true,
        forceHomeInstall: true,
        copyClaudeMd: false,
        copySettings: false,
        externalDepTypes: ['npx-skills', 'copilot-skills', 'agent-skills'],
        packageMappings: {
            'skills': 'skills',
            'utilities/reflections': 'reflections'
        },
        projectRootCopies: ['AGENTS.md'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.sh': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.py': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.js': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.ts': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.json': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.yml': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.toml': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            }
        }
    },
    'hermes-agent': {
        ruleDir: 'packages',
        targetSubdir: '.hermes',
        usePackagesStructure: true,
        forceHomeInstall: true,
        copyClaudeMd: false,
        copySettings: false,
        // Hermes reads skills from `~/.claude/skills/` via its own
        // `skills.external_dirs` config (see ~/.hermes/config.yaml), so any
        // skill installed for claude-code-4.5 is automatically discoverable
        // by hermes. To avoid redundant double-installs we keep
        // externalDepTypes empty here — entries that should reach hermes
        // simply need to apply to `claude`.
        externalDepTypes: [],
        packageMappings: {
            // Hermes uses nested skills/{category}/{skill-name}/SKILL.md layout.
            // Install toolkit skills under a 'toolkit/' category to avoid
            // collisions with hermes's native categories (apple, research, etc.)
            // and to clearly identify toolkit-owned skills.
            'skills': 'skills/toolkit'
        },
        templateSubstitutions: {
            '**/*.md':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' },
            '**/*.sh':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' },
            '**/*.py':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' },
            '**/*.js':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' },
            '**/*.ts':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' },
            '**/*.json': { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' },
            '**/*.yaml': { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' },
            '**/*.yml':  { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' },
            '**/*.toml': { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes' }
        }
    },
    nanoclaw: {
        ruleDir: 'packages',
        targetSubdir: '.claude',  // SHARED target with claude-code-4.5 (nanoclaw is a claude-code fork)
        usePackagesStructure: true,
        forceHomeInstall: true,
        // nanoclaw uses the same directory as claude-code-4.5, so copy the same stuff
        externalDepTypes: ['npx-skills', 'agent-skills'],  // skip claude-plugins (nanoclaw syncs from container/)
        packageMappings: {
            'skills': 'skills',
            'agents': 'agents',
            'utilities/utils': 'utils',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        templateSubstitutions: {
            '**/*.md':   { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' },
            '**/*.sh':   { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' },
            '**/*.py':   { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' },
            '**/*.js':   { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' },
            '**/*.ts':   { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' },
            '**/*.json': { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' },
            '**/*.yaml': { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' },
            '**/*.yml':  { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' },
            '**/*.toml': { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude' }
        }
    },
    'packages': {
        ruleDir: 'packages',
        targetSubdir: '.claude',
        copyEntireFolder: true,
        usePackagesStructure: true,
        packageMappings: {
            'skills': 'skills',
            'agents': 'agents',
            'utilities/utils': 'utils',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        excludeFiles: [],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.sh': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.py': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.js': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.ts': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.json': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.yml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.toml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            }
        }
    },
    gemini: {
        ruleGlob: 'GEMINI.md',
        ruleDir: 'gemini',
        targetSubdir: '.gemini',
        usePackagesStructure: true,
        forceHomeInstall: true,
        copyClaudeMd: false,
        copySettings: false,
        externalDepTypes: ['npx-skills', 'agent-skills'],
        packageMappings: {
            'skills': 'skills',
            '../gemini/agents': 'agents',
            'utilities/utils': 'utils',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        toolSpecificFiles: ['gemini/GEMINI.md', 'gemini/settings.json'],
        excludeFiles: ['settings.local.json'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.sh': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.py': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.js': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.ts': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.json': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.yml': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.toml': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            }
        }
    },
    amazonq: {
        ruleGlob: 'q-rulestore-rule.md',
        ruleDir: 'amazonq',
        targetSubdir: '.amazonq/rules',
        mcpFile: 'amazonq/mcp.json',
        mcpTarget: '.amazonq/mcp.json',
        usePackagesStructure: true,
        packageMappings: {
            'skills': 'skills',
            'agents': 'agents',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        toolSpecificFiles: ['amazonq/AmazonQ.md'],
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
        excludeFiles: ['settings.local.json'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.amazonq',
                'HOME_TOOL_DIR': '.amazonq'
            },
            '**/*.sh': {
                'TOOL_DIR': '.amazonq',
                'HOME_TOOL_DIR': '.amazonq'
            },
            '**/*.py': {
                'TOOL_DIR': '.amazonq',
                'HOME_TOOL_DIR': '.amazonq'
            }
        }
    },
    clawdhub: {
        ruleDir: 'clawdhub-skills',
        targetSubdir: 'skills',
        useClawdhubStructure: true,
        excludeFiles: ['_meta.json'],  // Don't copy auto-generated ClawdHub metadata
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.clawdbot',
                'HOME_TOOL_DIR': '~/.clawdbot'
            }
        }
    },
};

// Maps internal tool IDs to canonical names used in applies-to fields of the manifest
const TOOL_CANONICAL_NAMES = {
    'claude-code-4.5': 'claude',
};

const GENERAL_RULES_DIR = path.join(__dirname, 'general-rules');
const ALWAYS_COPY_RULES = [
    'rule-interpreter-rule.md',
    'rulestyle-rule.md',
];

// Discover available packages for interactive selection
function discoverPackages(packagesDir) {
    const packages = {
        skills: [],
        agentCategories: [],
        utilities: [],
        externalPlugins: [],
        npxSkills: []
    };

    // Read external dependencies manifest
    const manifestPath = path.join(path.dirname(packagesDir), 'external-dependencies.yaml');
    if (fs.existsSync(manifestPath)) {
        try {
            const manifestContent = fs.readFileSync(manifestPath, 'utf8');
            const manifest = yaml.load(manifestContent);

            // Claude plugins
            if (manifest['claude-plugins'] && Array.isArray(manifest['claude-plugins'])) {
                for (const plugin of manifest['claude-plugins']) {
                    packages.externalPlugins.push({
                        name: plugin.name,
                        marketplace: plugin.marketplace,
                        version: plugin.version || 'latest',
                        purpose: plugin.purpose || plugin.name,
                        install: plugin.install
                    });
                }
            }

            // npx skills
            if (manifest['npx-skills'] && Array.isArray(manifest['npx-skills']) && manifest['npx-skills'].length > 0) {
                for (const skill of manifest['npx-skills']) {
                    packages.npxSkills.push({
                        name: skill.name,
                        repo: skill.repo,
                        purpose: skill.purpose || skill.name,
                        install: skill.install
                    });
                }
            }
        } catch (err) {
            // Silently ignore manifest parse errors
        }
    }

    // Discover skills (each skill is individually selectable)
    const skillsDir = path.join(packagesDir, 'skills');
    if (fs.existsSync(skillsDir)) {
        const skillDirs = readdirSync(skillsDir).filter(f =>
            statSync(path.join(skillsDir, f)).isDirectory()
        );
        for (const skill of skillDirs) {
            const skillPath = path.join(skillsDir, skill);
            const skillMd = path.join(skillPath, 'SKILL.md');
            let description = skill;
            if (fs.existsSync(skillMd)) {
                const content = fs.readFileSync(skillMd, 'utf8');
                const descMatch = content.match(/description:\s*["']?([^"'\n]+)/);
                if (descMatch) description = descMatch[1].trim();
            }
            packages.skills.push({ name: skill, path: `skills/${skill}`, description });
        }
    }

    // Discover agent categories
    const agentsDir = path.join(packagesDir, 'agents');
    if (fs.existsSync(agentsDir)) {
        const categories = readdirSync(agentsDir).filter(f =>
            statSync(path.join(agentsDir, f)).isDirectory()
        );
        for (const cat of categories) {
            const catPath = path.join(agentsDir, cat);
            const agentFiles = readdirSync(catPath).filter(f => f.endsWith('.md'));
            packages.agentCategories.push({
                name: cat,
                path: `agents/${cat}`,
                count: agentFiles.length,
                agents: agentFiles.map(f => f.replace('.md', ''))
            });
        }
    }

    // Discover other utilities
    const utilityDirs = ['hooks', 'output-styles', 'reflections'];
    for (const util of utilityDirs) {
        const utilDir = path.join(packagesDir, 'utilities', util);
        if (fs.existsSync(utilDir)) {
            const files = readdirSync(utilDir);
            packages.utilities.push({ name: util, path: `utilities/${util}`, count: files.length });
        }
    }

    return packages;
}

// Build choices for interactive selection
function buildPackageChoices(packages) {
    const choices = [];

    // Skills section
    if (packages.skills.length > 0) {
        choices.push(new inquirer.Separator('─── Skills ───'));
        for (const skill of packages.skills) {
            choices.push({
                name: `${skill.name} - ${skill.description}`,
                value: { type: 'skill', name: skill.name, path: skill.path },
                checked: true
            });
        }
    }

    // Agent categories section
    if (packages.agentCategories.length > 0) {
        choices.push(new inquirer.Separator('─── Agents ───'));
        for (const cat of packages.agentCategories) {
            choices.push({
                name: `${cat.name} (${cat.count} agents: ${cat.agents.slice(0, 3).join(', ')}${cat.count > 3 ? '...' : ''})`,
                value: { type: 'agents', name: cat.name, path: cat.path },
                checked: true
            });
        }
    }

    // Utilities section
    if (packages.utilities.length > 0) {
        choices.push(new inquirer.Separator('─── Utilities ───'));
        for (const util of packages.utilities) {
            choices.push({
                name: `${util.name} (${util.count} files)`,
                value: { type: 'utility', name: util.name, path: util.path },
                checked: true
            });
        }
    }

    // External Claude plugins section
    if (packages.externalPlugins.length > 0) {
        choices.push(new inquirer.Separator('─── External Plugins (claude plugin) ───'));
        for (const plugin of packages.externalPlugins) {
            choices.push({
                name: `${plugin.name} - ${plugin.purpose} [${plugin.marketplace}]`,
                value: { type: 'external-plugin', name: plugin.name, marketplace: plugin.marketplace, install: plugin.install },
                checked: false  // External deps unchecked by default
            });
        }
    }

    // npx skills section
    if (packages.npxSkills.length > 0) {
        choices.push(new inquirer.Separator('─── npx Skills ───'));
        for (const skill of packages.npxSkills) {
            choices.push({
                name: `${skill.name} - ${skill.purpose} [${skill.repo}]`,
                value: { type: 'npx-skill', name: skill.name, repo: skill.repo, install: skill.install },
                checked: false  // External deps unchecked by default
            });
        }
    }

    return choices;
}

function getGeneralRuleFiles() {
    return readdirSync(GENERAL_RULES_DIR)
        .filter(f => f.endsWith('.md'))
        .filter(f => !ALWAYS_COPY_RULES.includes(f));
}

function parseFrontMatter(filePath) {
    const content = fs.readFileSync(filePath, 'utf8');
    const match = content.match(/^---([\s\S]*?)---/);
    if (!match) return {};
    try {
        return yaml.load(match[1]);
    } catch {
        return {};
    }
}

function isExternalSourcedSkill(skillDir) {
    const skillMd = path.join(skillDir, 'SKILL.md');
    if (!fs.existsSync(skillMd)) return false;
    const content = fs.readFileSync(skillMd, 'utf8');
    const match = content.match(/^---([\s\S]*?)---/);
    if (!match) return false;
    try {
        const fm = yaml.load(match[1]);
        return !!(fm && fm['external-source']);
    } catch { return false; }
}

function validateSkillFrontmatter(skillsDir) {
    const errors = [];

    function walk(dir) {
        const items = readdirSync(dir);
        for (const item of items) {
            const fullPath = path.join(dir, item);
            const isDirectory = statSync(fullPath).isDirectory();
            if (isDirectory) {
                walk(fullPath);
                continue;
            }
            if (item !== 'SKILL.md') {
                continue;
            }

            const content = fs.readFileSync(fullPath, 'utf8');
            const match = content.match(/^---([\s\S]*?)---/);
            if (!match) {
                errors.push(`${fullPath}: missing YAML frontmatter`);
                continue;
            }
            try {
                yaml.load(match[1]);
            } catch (err) {
                errors.push(`${fullPath}: ${err.message}`);
            }
        }
    }

    if (fs.existsSync(skillsDir)) {
        walk(skillsDir);
    }

    if (errors.length > 0) {
        const errorMessage = `Invalid SKILL.md YAML frontmatter:\n- ${errors.join('\n- ')}`;
        throw new Error(errorMessage);
    }
}

function showProgress(message, isComplete = false) {
    const greenCheck = '\x1b[32m✓\x1b[0m';
    
    if (isComplete) {
        console.log(`${greenCheck} ${message}`);
    } else {
        process.stdout.write(`⠋ ${message}...`);
    }
}

function completeProgress(message) {
    process.stdout.write('\r\x1b[K'); // Clear the line
    showProgress(message, true);
}

function substituteTemplate(content, substitutions) {
    let result = content;
    for (const [placeholder, value] of Object.entries(substitutions)) {
        const regex = new RegExp(`{{${placeholder}}}`, 'g');
        result = result.replace(regex, value);
    }
    return result;
}

function getEffectiveExcludeFiles(tool, config) {
    const excludeFiles = [...(config.excludeFiles || [])];

    // Exclude agents folder for all tools except claude-code-4.5
    if (tool !== 'claude-code-4.5') {
        excludeFiles.push('agents');
    }

    return excludeFiles;
}

function copyDirectoryRecursive(source, destination, excludeFiles = [], templateSubstitutions = {}) {
    const files = [];

    function shouldSkipItem(item, relativePath, isDirectory) {
        // Skip node_modules and logs directories
        if (isDirectory && (item === 'node_modules' || item === 'logs')) {
            return true;
        }

        // Skip build artifacts and lock files
        if (!isDirectory) {
            // Skip Bun build temp files
            if (item.startsWith('.') && item.includes('.bun-build')) {
                return true;
            }
            // Skip lock files in bin directories
            if (relativePath.includes('/bin/') && (item === 'bun.lockb' || item === 'bun.lock')) {
                return true;
            }
        }

        // Check explicit excludes
        return excludeFiles.some(excludeFile =>
            relativePath === excludeFile || item === excludeFile
        );
    }

    function getAllFiles(dir, basePath = '') {
        const items = readdirSync(dir);
        for (const item of items) {
            const fullPath = path.join(dir, item);
            const relativePath = path.join(basePath, item);
            const isDirectory = statSync(fullPath).isDirectory();

            if (shouldSkipItem(item, relativePath, isDirectory)) {
                continue;
            }

            if (isDirectory) {
                getAllFiles(fullPath, relativePath);
            } else {
                files.push({ source: fullPath, dest: path.join(destination, relativePath), fileName: item, relativePath: relativePath });
            }
        }
    }
    
    getAllFiles(source);
    
    for (const file of files) {
        const destDir = path.dirname(file.dest);
        fs.mkdirSync(destDir, { recursive: true });
        
        // Check if this file needs template substitution
        // Support both exact filename and relative path matching
        let substitutions = templateSubstitutions[file.fileName] || 
                            templateSubstitutions[file.relativePath] || 
                            (file.relativePath.endsWith('.md') ? templateSubstitutions['**/*.md'] : null);

        if (!substitutions) {
            for (const [pattern, patternSubs] of Object.entries(templateSubstitutions)) {
                if (pattern.startsWith('**/*.') && file.relativePath.endsWith(pattern.slice(4))) {
                    substitutions = patternSubs;
                    break;
                }
            }
        }
        
        if (substitutions) {
            let content = fs.readFileSync(file.source, 'utf8');
            content = substituteTemplate(content, substitutions);
            fs.writeFileSync(file.dest, content);
        } else {
            fs.copyFileSync(file.source, file.dest);
        }
    }
    
    return files.length;
}

// Deploy the global-learnings CLI to ~/.learnings/cli/ so every tool
// install (claude, codex, copilot, …) picks up the current CLI from
// ai-coder-rules — the single source of truth. The content repo
// (learnings-kb) holds only documents/indexes; its cli/ folder is
// deprecated and being removed.
//
// Strategy: copy every file from the template, then prune any file in
// the destination that isn't in the template. This removes stale CLI
// files from old learnings-kb clones without touching sibling data
// dirs (documents/, nano_graphrag_cache/, .venv/) — those live one
// level up at ~/.learnings/. Silent no-op if the template is missing.
//
// Returns {copied, pruned} counts.
function installLearningsCli() {
    const srcDir = path.join(__dirname, 'packages', 'knowledge', 'global-learnings-template', 'cli');
    if (!fs.existsSync(srcDir)) return { copied: 0, pruned: 0 };

    const destDir = path.join(os.homedir(), '.learnings', 'cli');
    fs.mkdirSync(destDir, { recursive: true });

    // Canonical fileset from the template — top-level files only.
    const templateFiles = new Set(
        readdirSync(srcDir).filter(e => !statSync(path.join(srcDir, e)).isDirectory())
    );

    // Copy canonical files.
    let copied = 0;
    for (const entry of templateFiles) {
        const src = path.join(srcDir, entry);
        const dst = path.join(destDir, entry);
        fs.copyFileSync(src, dst);
        if (entry === 'learnings') fs.chmodSync(dst, 0o755);
        copied++;
    }

    // Prune orphan files — anything in destDir not in templateFiles.
    // Keep subdirectories (__pycache__, etc.) alone; the wrapper
    // regenerates them.
    let pruned = 0;
    for (const entry of readdirSync(destDir)) {
        if (templateFiles.has(entry)) continue;
        const victim = path.join(destDir, entry);
        if (statSync(victim).isDirectory()) continue;
        fs.unlinkSync(victim);
        pruned++;
    }

    return { copied, pruned };
}

async function handleSharedContentCopy(tool, config, targetFolder) {
    if (!targetFolder) {
        const answers = await inquirer.prompt([
            {
                type: 'input',
                name: 'targetFolder',
                message: 'Enter the target project folder:',
                validate: (input) => !!input.trim() || 'Folder name required',
            },
        ]);
        targetFolder = answers.targetFolder;
    }

    showProgress('Creating target directory');
    if (!fs.existsSync(targetFolder)) {
        fs.mkdirSync(targetFolder, { recursive: true });
        completeProgress(`Created folder: ${targetFolder}`);
    } else {
        completeProgress(`Using existing folder: ${targetFolder}`);
    }

    const destDir = path.join(targetFolder, config.targetSubdir);
    fs.mkdirSync(destDir, { recursive: true });

    // Copy shared content from claude-code
    showProgress('Copying shared content from claude-code');
    const sharedSourceDir = path.join(__dirname, config.sharedContentDir);
    
    const excludeFiles = getEffectiveExcludeFiles(tool, config);
    const sharedFilesCopied = copyDirectoryRecursive(sharedSourceDir, destDir, excludeFiles, config.templateSubstitutions || {});
    completeProgress(`Copied ${sharedFilesCopied} shared files`);

    // Copy tool-specific files
    showProgress('Copying tool-specific files');
    const toolSpecificPath = path.join(__dirname, config.ruleDir, config.ruleGlob);
    if (fs.existsSync(toolSpecificPath)) {
        // For GEMINI.md, copy CLAUDE.md and apply substitutions
        if (config.ruleGlob === 'GEMINI.md') {
            const claudePath = path.join(__dirname, 'claude-code-4.5', 'CLAUDE.md');
            let content = fs.readFileSync(claudePath, 'utf8');
            if (config.templateSubstitutions && config.templateSubstitutions['GEMINI.md']) {
                content = substituteTemplate(content, config.templateSubstitutions['GEMINI.md']);
            }
            fs.writeFileSync(path.join(destDir, config.ruleGlob), content);
        } else {
            fs.copyFileSync(toolSpecificPath, path.join(destDir, config.ruleGlob));
        }
        completeProgress('Copied tool-specific files');
    }

    // Copy always copy rules
    showProgress('Copying core rules');
    for (const rule of ALWAYS_COPY_RULES) {
        fs.copyFileSync(path.join(GENERAL_RULES_DIR, rule), path.join(destDir, rule));
    }
    completeProgress('Copied core rules');

    // Copy shared content to specific target if specified
    if (config.sharedContentTarget) {
        const targetDir = path.join(targetFolder, config.sharedContentTarget);
        showProgress(`Copying shared content to ${config.sharedContentTarget}`);
        const sharedSourceDir = path.join(__dirname, config.sharedContentDir);
        fs.mkdirSync(targetDir, { recursive: true });
        const excludeFiles = getEffectiveExcludeFiles(tool, config);
        const sharedFilesCopied = copyDirectoryRecursive(sharedSourceDir, targetDir, excludeFiles, config.templateSubstitutions || {});
        completeProgress(`Copied ${sharedFilesCopied} shared files to ${config.sharedContentTarget}`);
    }

    // Copy settings file to project directory if specified
    if (config.settingsFile) {
        showProgress('Copying settings file');
        const sourcePath = path.join(__dirname, config.settingsFile);
        const destPath = path.join(destDir, 'settings.json');
        
        if (fs.existsSync(sourcePath)) {
            fs.copyFileSync(sourcePath, destPath);
            completeProgress('Copied settings file');
        }
    }

    // Copy MCP file to specified target if specified
    if (config.mcpFile && config.mcpTarget) {
        showProgress('Copying MCP configuration');
        const sourcePath = path.join(__dirname, config.mcpFile);
        const destPath = path.join(targetFolder, config.mcpTarget);
        
        if (fs.existsSync(sourcePath)) {
            fs.mkdirSync(path.dirname(destPath), { recursive: true });
            fs.copyFileSync(sourcePath, destPath);
            completeProgress(`Copied MCP config to ${config.mcpTarget}`);
        }
    }

    // Perform special file copies
    if (config.specialCopies) {
        showProgress('Performing special file copies');
        let specialFilesCopied = 0;
        for (const copy of config.specialCopies) {
            const sourcePath = path.join(__dirname, copy.source);
            const destPath = path.join(targetFolder, copy.dest);
            const fileName = path.basename(copy.source);

            if (fs.existsSync(sourcePath)) {
                fs.mkdirSync(path.dirname(destPath), { recursive: true });

                if (config.templateSubstitutions && config.templateSubstitutions[fileName]) {
                    let content = fs.readFileSync(sourcePath, 'utf8');
                    content = substituteTemplate(content, config.templateSubstitutions[fileName]);
                    fs.writeFileSync(destPath, content);
                } else {
                    fs.copyFileSync(sourcePath, destPath);
                }
                specialFilesCopied++;
            }
        }
        completeProgress(`Copied ${specialFilesCopied} special files`);
    }

    

    // Create linked files if they exist
    if (config.linkedFiles) {
        showProgress('Creating linked files');
        let linkedFilesCreated = 0;
        for (const link of config.linkedFiles) {
            const linkPath = path.join(targetFolder, link.linkName);
            const sourcePath = path.join(config.targetSubdir, link.source.split('/').pop());
            fs.writeFileSync(linkPath, `@${sourcePath}`);
            linkedFilesCreated++;
        }
        completeProgress(`Created ${linkedFilesCreated} linked files`);
    }

    console.log(`\n\x1b[32m🎉 ${tool} setup complete!\x1b[0m`);
    console.log(`Files copied to: ${destDir}`);
}

async function handlePackagesStructureCopy(tool, config, overrideHomeDir = null, targetFolder = null, isNonInteractive = false, specifiedPackages = null) {
    let destDir;
    let displayPath;

    const shouldUseHome = !targetFolder || config.forceHomeInstall;
    if (!shouldUseHome) {
        destDir = path.join(targetFolder, config.targetSubdir);
        displayPath = path.join(targetFolder, config.targetSubdir);
    } else {
        const homeDir = overrideHomeDir || os.homedir();
        destDir = path.join(homeDir, config.targetSubdir);
        displayPath = `~/${config.targetSubdir}`;
    }

    const packagesDir = path.join(__dirname, 'packages');
    let totalFilesCopied = 0;

    // Package selection for project installations (not home directory)
    let selectedPackagePaths = null;

    // If packages specified via CLI, use those
    if (!shouldUseHome && specifiedPackages) {
        selectedPackagePaths = new Set(specifiedPackages);
        console.log(`\n📦 Installing specified packages: ${specifiedPackages.join(', ')}\n`);
    }
    // Interactive package selection for project installations
    else if (!shouldUseHome && !isNonInteractive) {
        const availablePackages = discoverPackages(packagesDir);
        const choices = buildPackageChoices(availablePackages);

        console.log('\n📦 Select packages to install in your project:\n');
        const { selectedPackages } = await inquirer.prompt([
            {
                type: 'checkbox',
                name: 'selectedPackages',
                message: 'Use space to toggle, enter to confirm:',
                choices: choices,
                pageSize: 20
            }
        ]);

        // Build list of selected paths and external deps
        selectedPackagePaths = new Set();
        const selectedExternalDeps = [];

        for (const pkg of selectedPackages) {
            if (pkg.type === 'skill') {
                selectedPackagePaths.add(pkg.path);
            } else if (pkg.type === 'agents') {
                selectedPackagePaths.add(pkg.path);
            } else if (pkg.type === 'utility') {
                selectedPackagePaths.add(pkg.path);
            } else if (pkg.type === 'external-plugin' || pkg.type === 'npx-skill') {
                selectedExternalDeps.push(pkg);
            }
        }

        // Generate setup-external.sh if external deps were selected
        if (selectedExternalDeps.length > 0) {
            const scriptLines = [
                '#!/bin/bash',
                '# External dependencies for this project',
                '# Generated by create-rule.js',
                `# Run from project root: bash ${config.targetSubdir}/setup-external.sh`,
                '#',
                '# Plugins are installed at PROJECT scope (not user scope)',
                '# This avoids conflicts with user-level plugin installations',
                '',
                'set -e',
                '',
                '# Ensure we are in the project directory',
                'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"',
                'PROJECT_DIR="$(dirname "$SCRIPT_DIR")"',
                'cd "$PROJECT_DIR"',
                ''
            ];

            const plugins = selectedExternalDeps.filter(d => d.type === 'external-plugin');
            const npxSkills = selectedExternalDeps.filter(d => d.type === 'npx-skill');

            if (plugins.length > 0) {
                scriptLines.push('echo "Installing Claude plugins (project scope)..."');
                scriptLines.push('');

                // Collect unique marketplaces
                const marketplaces = new Set();
                for (const plugin of plugins) {
                    if (plugin.marketplace) {
                        marketplaces.add(plugin.marketplace);
                    }
                }

                // Add marketplaces first (skip if already exists)
                for (const marketplace of marketplaces) {
                    scriptLines.push(`claude plugin marketplace add ${marketplace} 2>/dev/null || true`);
                }
                scriptLines.push('');

                // Install plugins at project scope
                for (const plugin of plugins) {
                    scriptLines.push(`claude plugin install ${plugin.name} --scope project`);
                }
                scriptLines.push('');
            }

            if (npxSkills.length > 0) {
                scriptLines.push('echo "Installing npx skills..."');
                scriptLines.push('');
                for (const skill of npxSkills) {
                    scriptLines.push(skill.install || `npx skills add ${skill.repo}`);
                }
                scriptLines.push('');
            }

            scriptLines.push('echo "✓ External dependencies installed!"');

            // Store for later writing (after destDir is created)
            config._externalDepsScript = scriptLines.join('\n');
            config._externalDepsCount = selectedExternalDeps.length;
        }

        if (selectedPackagePaths.size === 0 && selectedExternalDeps.length === 0) {
            console.log('\n⚠️  No packages selected. Only copying core files.\n');
        }
    }
    // Generate setup-external.sh for home directory installs (codex, copilot, claude-code-4.5)
    else if (shouldUseHome && config.externalDepTypes && config.externalDepTypes.length > 0) {
        const manifestPath = path.join(path.dirname(packagesDir), 'external-dependencies.yaml');
        if (fs.existsSync(manifestPath)) {
            try {
                const manifestContent = fs.readFileSync(manifestPath, 'utf8');
                const manifest = yaml.load(manifestContent);

                const applicableDeps = [];
                for (const depType of config.externalDepTypes) {
                    if (manifest[depType] && Array.isArray(manifest[depType])) {
                        for (const dep of manifest[depType]) {
                            applicableDeps.push({ ...dep, _depType: depType });
                        }
                    }
                }

                if (applicableDeps.length > 0) {
                    const toolName = tool;
                    const scriptLines = [
                        '#!/bin/bash',
                        `# External marketplace skills and plugins for ${toolName}`,
                        '# Generated by create-rule.js',
                        `# Run: bash ~/${config.targetSubdir}/setup-external.sh`,
                        '',
                        'set -e',
                        ''
                    ];

                    const claudePlugins = applicableDeps.filter(d => d._depType === 'claude-plugins');
                    const npxSkills = applicableDeps.filter(d => d._depType === 'npx-skills');
                    const agentSkills = applicableDeps.filter(d => d._depType === 'agent-skills');
                    const otherSkills = applicableDeps.filter(d => d._depType !== 'claude-plugins' && d._depType !== 'npx-skills' && d._depType !== 'agent-skills');

                    if (claudePlugins.length > 0) {
                        scriptLines.push('echo "Installing Claude plugins..."');
                        scriptLines.push('');
                        const marketplaces = new Set(claudePlugins.map(p => p.marketplace).filter(Boolean));
                        for (const m of marketplaces) {
                            scriptLines.push(`claude plugin marketplace add ${m} 2>/dev/null || true`);
                        }
                        scriptLines.push('');
                        for (const p of claudePlugins) {
                            scriptLines.push(`claude plugin install ${p.name}`);
                        }
                        scriptLines.push('');
                        // Copy plugin skills into ~/.claude/skills/ so they're discoverable as slash commands
                        // Plugin installs put skills in ~/.claude/plugins/cache/<marketplace_id>/<name>/<version>/skills/
                        // but Claude Code only looks in ~/.claude/skills/ for user-invocable skills
                        scriptLines.push('echo "Copying plugin skills to skills directory..."');
                        for (const p of claudePlugins) {
                            const marketplaceId = p.marketplace_id || `${p.name}-marketplace`;
                            scriptLines.push(`for skill_dir in "\${HOME}/.claude/plugins/cache/${marketplaceId}/${p.name}"/*/skills/*/; do`);
                            scriptLines.push(`  if [ -d "$skill_dir" ] && [ -f "$skill_dir/SKILL.md" ]; then`);
                            scriptLines.push(`    skill_name=$(basename "$skill_dir")`);
                            scriptLines.push(`    if [ ! -d "\${HOME}/${config.targetSubdir}/skills/$skill_name" ]; then`);
                            scriptLines.push(`      cp -R "$skill_dir" "\${HOME}/${config.targetSubdir}/skills/$skill_name"`);
                            scriptLines.push(`      echo "  Copied plugin skill: $skill_name"`);
                            scriptLines.push(`    fi`);
                            scriptLines.push(`  fi`);
                            scriptLines.push(`done`);
                        }
                        scriptLines.push('');
                    }

                    if (npxSkills.length > 0) {
                        // Filter out catalog-only entries (aggregate bundles with
                        // no install command or repo — e.g. gws-skills, browserbase-skills).
                        // These live in the manifest for discoverability only.
                        const installable = npxSkills.filter(s => s.install || s.repo);
                        if (installable.length > 0) {
                            scriptLines.push('echo "Installing npx agent skills..."');
                            scriptLines.push('');
                            for (const s of installable) {
                                // Prefer explicit install command; fall back to modern
                                // `npx skills add ... --yes` (non-interactive) rather than
                                // the deprecated interactive `npx add-skill`.
                                scriptLines.push(s.install || `npx skills add ${s.repo} --yes`);
                            }
                            scriptLines.push('');
                        }
                    }

                    // Filter agent-skills by applies-to field
                    // Use canonical name so e.g. 'claude-code-4.5' matches 'claude' in applies-to
                    // Skills with `catalog-only: true` are listed in the manifest for discoverability
                    // but never installed by bootstrap.
                    const canonicalToolName = TOOL_CANONICAL_NAMES[toolName] || toolName;
                    const relevantAgentSkills = agentSkills.filter(skill => {
                        if (skill['catalog-only']) return false;
                        // Safety: skip entries without a clone source (git repo).
                        // Some entries (e.g. clawhub/pip distributions) list only
                        // `source:` for documentation — they can't be git-cloned.
                        if (!skill.repo) return false;
                        if (!skill['applies-to']) return true;
                        return skill['applies-to'].includes(toolName) || skill['applies-to'].includes(canonicalToolName);
                    });

                    if (relevantAgentSkills.length > 0) {
                        scriptLines.push('echo "Installing agent skills (git repos)..."');
                        scriptLines.push('');
                        // Tools can override where external agent-skills are
                        // installed relative to targetSubdir. Default: `skills/`.
                        const externalSkillsPath = config.externalSkillsSubpath || 'skills';
                        for (const skill of relevantAgentSkills) {
                            const skillDir = `"\${HOME}/${config.targetSubdir}/${externalSkillsPath}/${skill.name}"`;
                            scriptLines.push(`# ${skill.name}${skill.purpose ? ' - ' + skill.purpose : ''}`);
                            scriptLines.push(`SKILL_DIR=${skillDir}`);
                            if (skill['multi-subpath']) {
                                // Repo bundles multiple sibling skills under <multi-subpath>/.
                                // Clone repo to a temp dir, then for each subdir under
                                // <multi-subpath>/ that contains SKILL.md, copy the whole
                                // subdir into externalSkillsDir/<subdir-name>/ as a flat
                                // sibling. Idempotent: skip any target that already has
                                // SKILL.md.
                                const extBase = `"\${HOME}/${config.targetSubdir}/${externalSkillsPath}"`;
                                scriptLines.push(`echo "  Installing ${skill.name} bundle (multi-subpath: ${skill['multi-subpath']})..."`);
                                scriptLines.push(`TMP_CLONE=$(mktemp -d)`);
                                scriptLines.push(`git clone --depth 1 ${skill.repo} "$TMP_CLONE/repo"`);
                                scriptLines.push(`mkdir -p ${extBase}`);
                                scriptLines.push(`for sub in "$TMP_CLONE/repo/${skill['multi-subpath']}"/*/; do`);
                                scriptLines.push(`  sub_name=$(basename "$sub")`);
                                scriptLines.push(`  if [ -f "$sub/SKILL.md" ]; then`);
                                scriptLines.push(`    if [ -f ${extBase}/"$sub_name"/SKILL.md ]; then`);
                                scriptLines.push(`      echo "    $sub_name already installed (skipping)"`);
                                scriptLines.push(`    else`);
                                scriptLines.push(`      mkdir -p ${extBase}/"$sub_name"`);
                                scriptLines.push(`      cp -R "$sub"/. ${extBase}/"$sub_name"/`);
                                scriptLines.push(`      echo "    Installed $sub_name"`);
                                scriptLines.push(`    fi`);
                                scriptLines.push(`  fi`);
                                scriptLines.push(`done`);
                                scriptLines.push(`rm -rf "$TMP_CLONE"`);
                            } else if (skill.subpath) {
                                // Repo contains the skill under a subdirectory. Clone to a temp
                                // location and copy just the subpath into the skill dir so the
                                // target ends up with SKILL.md at its root.
                                scriptLines.push(`if [ -d "\${SKILL_DIR}" ] && [ -f "\${SKILL_DIR}/SKILL.md" ]; then`);
                                scriptLines.push(`  echo "  ${skill.name} already installed (skipping)"`);
                                scriptLines.push(`else`);
                                scriptLines.push(`  echo "  Installing ${skill.name} (subpath: ${skill.subpath})..."`);
                                scriptLines.push(`  TMP_CLONE=$(mktemp -d)`);
                                scriptLines.push(`  git clone --depth 1 ${skill.repo} "$TMP_CLONE/repo"`);
                                scriptLines.push(`  mkdir -p "\${SKILL_DIR}"`);
                                scriptLines.push(`  cp -R "$TMP_CLONE/repo/${skill.subpath}/." "\${SKILL_DIR}/"`);
                                scriptLines.push(`  rm -rf "$TMP_CLONE"`);
                                scriptLines.push(`fi`);
                            } else {
                                scriptLines.push(`if [ -d "\${SKILL_DIR}/.git" ]; then`);
                                scriptLines.push(`  echo "  Updating ${skill.name}..."`);
                                scriptLines.push(`  git -C "\${SKILL_DIR}" pull --ff-only`);
                                scriptLines.push(`else`);
                                scriptLines.push(`  echo "  Installing ${skill.name}..."`);
                                scriptLines.push(`  mkdir -p "$(dirname "\${SKILL_DIR}")"`);
                                scriptLines.push(`  git clone ${skill.repo} "\${SKILL_DIR}"`);
                                scriptLines.push(`fi`);
                            }
                            scriptLines.push('');
                        }
                    }

                    if (otherSkills.length > 0) {
                        scriptLines.push('echo "Installing additional marketplace skills..."');
                        scriptLines.push('');
                        for (const s of otherSkills) {
                            if (s.install) scriptLines.push(s.install);
                        }
                        scriptLines.push('');
                    }

                    scriptLines.push('echo "✓ External dependencies installed!"');
                    config._externalDepsScript = scriptLines.join('\n');
                    config._externalDepsCount = applicableDeps.length;
                }
            } catch (err) {
                // Silently ignore manifest parse errors
            }
        }
    }

    showProgress(`Checking ${displayPath} directory`);
    if (!fs.existsSync(destDir)) {
        fs.mkdirSync(destDir, { recursive: true });
        completeProgress(`Created ${displayPath} directory`);
    } else {
        completeProgress(`Found ${displayPath} directory`);
    }

    if (config.validateSkillFrontmatter !== false && config.packageMappings && Object.prototype.hasOwnProperty.call(config.packageMappings, 'skills')) {
        showProgress('Validating SKILL.md frontmatter');
        try {
            validateSkillFrontmatter(path.join(packagesDir, 'skills'));
            completeProgress('Validated SKILL.md frontmatter');
        } catch (error) {
            completeProgress('SKILL.md frontmatter validation failed');
            throw error;
        }
    }

    // Copy using package mappings (filtered by selection if project install)
    for (const [source, target] of Object.entries(config.packageMappings)) {
        // If we have a selection, check if this source is selected
        if (selectedPackagePaths !== null) {
            // Check if this exact path or a parent path is selected
            const isSelected = [...selectedPackagePaths].some(sel => {
                // Exact match or source starts with selected path
                return source === sel || source.startsWith(sel + '/') || sel.startsWith(source + '/') || sel === source;
            });

            // Special handling for skills - copy individual skills
            if (source === 'skills') {
                const skillsSelected = [...selectedPackagePaths].filter(p => p.startsWith('skills/'));
                if (skillsSelected.length > 0) {
                    for (const skillPath of skillsSelected) {
                        const sourceDir = path.join(packagesDir, skillPath);
                        const skillName = skillPath.split('/')[1];
                        if (isExternalSourcedSkill(sourceDir)) {
                            continue; // Handled by setup-external.sh git clone
                        }
                        const targetDir = path.join(destDir, 'skills', skillName);
                        if (fs.existsSync(sourceDir)) {
                            showProgress(`Copying skill: ${skillName}`);
                            fs.mkdirSync(targetDir, { recursive: true });
                            const filesCopied = copyDirectoryRecursive(sourceDir, targetDir, config.excludeFiles || [], config.templateSubstitutions || {});
                            totalFilesCopied += filesCopied;
                            completeProgress(`Copied ${filesCopied} files from ${skillName}`);
                        }
                    }
                }
                continue; // Skip the default skills copy
            }

            // Special handling for agents - copy individual agent categories
            if (source === 'agents') {
                const agentsSelected = [...selectedPackagePaths].filter(p => p.startsWith('agents/'));
                if (agentsSelected.length > 0) {
                    for (const agentPath of agentsSelected) {
                        const sourceDir = path.join(packagesDir, agentPath);
                        const categoryName = agentPath.split('/')[1];
                        const targetDir = path.join(destDir, 'agents', categoryName);
                        if (fs.existsSync(sourceDir)) {
                            showProgress(`Copying agents: ${categoryName}`);
                            fs.mkdirSync(targetDir, { recursive: true });
                            const filesCopied = copyDirectoryRecursive(sourceDir, targetDir, config.excludeFiles || [], config.templateSubstitutions || {});
                            totalFilesCopied += filesCopied;
                            completeProgress(`Copied ${filesCopied} files from agents/${categoryName}`);
                        }
                    }
                }
                continue; // Skip the default agents copy
            }

            if (!isSelected) {
                continue; // Skip unselected packages
            }
        }

        const sourceDir = path.join(packagesDir, source);
        const targetDir = path.join(destDir, target);

        if (fs.existsSync(sourceDir)) {
            // For skills, copy individually so external-sourced skills can be skipped
            if (source === 'skills') {
                fs.mkdirSync(targetDir, { recursive: true });
                const skillDirs = readdirSync(sourceDir).filter(f =>
                    statSync(path.join(sourceDir, f)).isDirectory()
                );
                for (const skillName of skillDirs) {
                    const skillSrcDir = path.join(sourceDir, skillName);
                    if (isExternalSourcedSkill(skillSrcDir)) {
                        continue; // Handled by setup-external.sh git clone
                    }
                    showProgress(`Copying skill: ${skillName}`);
                    const skillDestDir = path.join(targetDir, skillName);
                    fs.mkdirSync(skillDestDir, { recursive: true });
                    const filesCopied = copyDirectoryRecursive(skillSrcDir, skillDestDir, config.excludeFiles || [], config.templateSubstitutions || {});
                    totalFilesCopied += filesCopied;
                    completeProgress(`Copied ${filesCopied} files from skills/${skillName}`);
                }
            } else {
                showProgress(`Copying ${source} to ${target}`);
                fs.mkdirSync(targetDir, { recursive: true });
                const filesCopied = copyDirectoryRecursive(sourceDir, targetDir, config.excludeFiles || [], config.templateSubstitutions || {});
                totalFilesCopied += filesCopied;
                completeProgress(`Copied ${filesCopied} files from ${source}`);
            }
        }
    }

    // Clean up stale commands and templates directories (deprecated - now skills)
    const staleCommandsDir = path.join(destDir, 'commands');
    const staleTemplatesDir = path.join(destDir, 'templates');
    const stalePromptsDir = path.join(destDir, 'prompts');
    for (const staleDir of [staleCommandsDir, staleTemplatesDir, stalePromptsDir]) {
        if (fs.existsSync(staleDir)) {
            fs.rmSync(staleDir, { recursive: true });
            console.log(`  Removed deprecated ${path.basename(staleDir)}/ directory`);
        }
    }

    // Copy CLAUDE.md from claude-code-4.5 if it exists
    // Skip for project folder installations to avoid overwriting project-specific CLAUDE.md
    if (config.copyClaudeMd !== false && shouldUseHome) {
        const claudeMdSource = path.join(__dirname, 'claude-code-4.5', 'CLAUDE.md');
        if (fs.existsSync(claudeMdSource)) {
            showProgress('Copying CLAUDE.md');
            let content = fs.readFileSync(claudeMdSource, 'utf8');
            if (config.templateSubstitutions && config.templateSubstitutions['**/*.md']) {
                content = substituteTemplate(content, config.templateSubstitutions['**/*.md']);
            }
            fs.writeFileSync(path.join(destDir, 'CLAUDE.md'), content);
            totalFilesCopied++;
            completeProgress('Copied CLAUDE.md');
        }
    } else if (!shouldUseHome) {
        console.log('\x1b[33m⚠\x1b[0m  Skipping CLAUDE.md (project folder - won\'t overwrite existing)');
    }

    // Copy settings.json from claude-code-4.5 if it exists
    // Skip for project folder installations to avoid overwriting project-specific settings
    if (config.copySettings !== false && shouldUseHome) {
        const settingsSource = path.join(__dirname, 'claude-code-4.5', 'settings.json');
        if (fs.existsSync(settingsSource)) {
            showProgress('Copying settings.json');
            fs.copyFileSync(settingsSource, path.join(destDir, 'settings.json'));
            totalFilesCopied++;
            completeProgress('Copied settings.json');
        }
    }

    if (config.toolSpecificFiles) {
        showProgress('Copying tool-specific files');
        let toolFilesCopied = 0;
        for (const toolFile of config.toolSpecificFiles) {
            const sourcePath = path.join(__dirname, toolFile);
            const fileName = path.basename(toolFile);
            const destPath = path.join(destDir, fileName);

            if (fs.existsSync(sourcePath)) {
                const substitutions = (config.templateSubstitutions || {})[fileName] ||
                    (fileName.endsWith('.md') ? (config.templateSubstitutions || {})['**/*.md'] : null);

                if (substitutions) {
                    let content = fs.readFileSync(sourcePath, 'utf8');
                    content = substituteTemplate(content, substitutions);
                    fs.writeFileSync(destPath, content);
                } else {
                    fs.copyFileSync(sourcePath, destPath);
                }
                toolFilesCopied++;
            }
        }
        completeProgress(`Copied ${toolFilesCopied} tool-specific files`);
    }

    if (config.projectRootCopies) {
        const rootTarget = targetFolder || (config.forceHomeInstall ? destDir : null);
        if (rootTarget) {
            if (!fs.existsSync(rootTarget)) {
                fs.mkdirSync(rootTarget, { recursive: true });
            }
        }
        showProgress('Copying project root files');
        let rootFilesCopied = 0;
        for (const fileName of config.projectRootCopies) {
            const sourcePath = path.join(__dirname, config.ruleDir, fileName);
            if (!fs.existsSync(sourcePath) || !rootTarget) {
                continue;
            }
            const destPath = path.join(rootTarget, fileName);

            // Ensure parent directory exists (supports subdirectory paths like .github/copilot-instructions.md)
            fs.mkdirSync(path.dirname(destPath), { recursive: true });

            const substitutions = (config.templateSubstitutions || {})[fileName] ||
                (fileName.endsWith('.md') ? (config.templateSubstitutions || {})['**/*.md'] : null);

            if (substitutions) {
                let content = fs.readFileSync(sourcePath, 'utf8');
                content = substituteTemplate(content, substitutions);
                fs.writeFileSync(destPath, content);
            } else {
                fs.copyFileSync(sourcePath, destPath);
            }
            rootFilesCopied++;
        }
        completeProgress(`Copied ${rootFilesCopied} project root files`);
    }

    // Write setup-external.sh if external deps were selected
    if (config._externalDepsScript) {
        const scriptPath = path.join(destDir, 'setup-external.sh');
        showProgress('Generating setup-external.sh');
        fs.writeFileSync(scriptPath, config._externalDepsScript);
        fs.chmodSync(scriptPath, 0o755);
        completeProgress(`Generated setup-external.sh (${config._externalDepsCount} external deps)`);
    }

    // Deploy the learnings CLI so ai-coder-rules is the single source of
    // truth for the tool, even though the KB content lives in a separate
    // repo (learnings-kb). Runs on every tool install; cheap and idempotent.
    if (shouldUseHome) {
        showProgress('Installing learnings CLI to ~/.learnings/cli/');
        const { copied, pruned } = installLearningsCli();
        if (copied > 0) {
            const prunedMsg = pruned > 0 ? `, pruned ${pruned} stale` : '';
            completeProgress(`Installed learnings CLI (${copied} files${prunedMsg}) to ~/.learnings/cli/`);
        } else {
            completeProgress('learnings CLI template not found — skipped');
        }
    }

    console.log(`\n\x1b[32m🎉 packages setup complete!\x1b[0m`);
    console.log(`Files copied to: ${destDir} (${totalFilesCopied} files)`);
    console.log(`\nPackages structure installed. Components available:`);
    console.log(`  - Skills: ${destDir}/skills/`);
    console.log(`  - Agents: ${destDir}/agents/`);
    console.log(`  - Hooks: ${destDir}/hooks/`);
    console.log(`  - Utils: ${destDir}/utils/`);

    // Show external deps instructions if script was generated
    if (config._externalDepsScript) {
        console.log(`\n\x1b[33m📦 External dependencies:\x1b[0m`);
        console.log(`  Run: bash ${destDir}/setup-external.sh`);
    }
}

async function handleFullDirectoryCopy(tool, config, overrideHomeDir = null, targetFolder = null) {
    let destDir;
    let displayPath;
    
    const shouldUseHome = !targetFolder || config.forceHomeInstall;
    if (!shouldUseHome) {
        // If targetFolder is specified, use it instead of home directory
        destDir = path.join(targetFolder, config.targetSubdir);
        displayPath = path.join(targetFolder, config.targetSubdir);
        showProgress(`Checking ${displayPath} directory`);
    } else {
        // Default behavior - use home directory
        const homeDir = overrideHomeDir || os.homedir();
        destDir = path.join(homeDir, config.targetSubdir);
        displayPath = `~/${config.targetSubdir}`;
        showProgress(`Checking ${displayPath} directory`);
    }
    
    if (!fs.existsSync(destDir)) {
        fs.mkdirSync(destDir, { recursive: true });
        completeProgress(`Created ${displayPath} directory`);
    } else {
        completeProgress(`Found ${displayPath} directory`);
    }

    showProgress(`Copying ${config.ruleDir} contents`);
    const sourceDir = path.join(__dirname, config.ruleDir);
    const filesCopied = copyDirectoryRecursive(sourceDir, destDir, config.excludeFiles || [], config.templateSubstitutions || {});
    completeProgress(`Copied ${filesCopied} files to ${displayPath}`);

    if (targetFolder && config.projectRootCopies) {
        if (!fs.existsSync(targetFolder)) {
            fs.mkdirSync(targetFolder, { recursive: true });
        }
        showProgress('Copying project root files');
        let rootFilesCopied = 0;
        for (const fileName of config.projectRootCopies) {
            const sourcePath = path.join(__dirname, config.ruleDir, fileName);
            const destPath = path.join(targetFolder, fileName);
            if (!fs.existsSync(sourcePath)) {
                continue;
            }

            const substitutions = (config.templateSubstitutions || {})[fileName] ||
                (fileName.endsWith('.md') ? (config.templateSubstitutions || {})['**/*.md'] : null);

            if (substitutions) {
                let content = fs.readFileSync(sourcePath, 'utf8');
                content = substituteTemplate(content, substitutions);
                fs.writeFileSync(destPath, content);
            } else {
                fs.copyFileSync(sourcePath, destPath);
            }
            rootFilesCopied++;
        }
        completeProgress(`Copied ${rootFilesCopied} project root files`);
    }

    // Copy skills folder for claude-code-4.5
    if (tool === 'claude-code-4.5') {
        showProgress('Copying skills folder');
        const skillsSource = path.join(__dirname, config.ruleDir, 'skills');
        const skillsDest = path.join(destDir, 'skills');

        if (fs.existsSync(skillsSource)) {
            const skillsFiles = copyDirectoryRecursive(skillsSource, skillsDest, [], config.templateSubstitutions || {});
            completeProgress(`Copied ${skillsFiles} skill files`);
        }

        // Smart compilation for browser-tools in webapp-testing skill
        const browserToolsDir = path.join(skillsDest, 'webapp-testing', 'bin');
        const browserToolsTs = path.join(browserToolsDir, 'browser-tools.ts');
        const browserToolsBinary = path.join(browserToolsDir, 'browser-tools');
        const rebuildFlag = process.argv.includes('--rebuild');

        if (fs.existsSync(browserToolsTs)) {
            // For claude-code-4.5: compile the binary
            if (tool === 'claude-code-4.5') {
                let shouldCompile = rebuildFlag;

                if (!fs.existsSync(browserToolsBinary)) {
                    shouldCompile = true;
                    showProgress('browser-tools binary missing, compiling');
                } else if (!rebuildFlag) {
                    const tsStats = statSync(browserToolsTs);
                    const binaryStats = statSync(browserToolsBinary);
                    if (tsStats.mtime > binaryStats.mtime) {
                        shouldCompile = true;
                        showProgress('browser-tools.ts is newer, recompiling');
                    }
                }

                if (shouldCompile) {
                    try {
                        showProgress('Compiling browser-tools binary');
                        const { execSync } = await import('child_process');

                        // Try Bun first
                        try {
                            execSync('which bun', { stdio: 'pipe' });
                            execSync(
                                `cd "${browserToolsDir}" && bun install && bun build browser-tools.ts --compile --target bun --outfile browser-tools`,
                                { stdio: 'pipe' }
                            );
                            completeProgress('Compiled browser-tools with Bun');
                        } catch {
                            // Fallback to esbuild
                            try {
                                execSync('which esbuild', { stdio: 'pipe' });
                                execSync(
                                    `cd "${browserToolsDir}" && npm install && esbuild browser-tools.ts --bundle --platform=node --outfile=browser-tools.js`,
                                    { stdio: 'pipe' }
                                );
                                completeProgress('Compiled browser-tools with esbuild (JavaScript output)');
                            } catch {
                                completeProgress('Compilation failed, using existing binary if available');
                            }
                        }
                    } catch (error) {
                        completeProgress(`Compilation error: ${error.message}, using existing binary if available`);
                    }
                }
            }
        }
    }

    // Copy tool-specific files if they exist
    if (config.toolSpecificFiles) {
        showProgress('Copying tool-specific files');
        let toolFilesCopied = 0;
        for (const toolFile of config.toolSpecificFiles) {
            const sourcePath = path.join(__dirname, toolFile);
            const fileName = path.basename(toolFile);
            const destPath = path.join(destDir, fileName);
            
            if (fs.existsSync(sourcePath)) {
                fs.copyFileSync(sourcePath, destPath);
                toolFilesCopied++;
            }
        }
        completeProgress(`Copied ${toolFilesCopied} tool-specific files`);
    }

    console.log(`\n\x1b[32m🎉 ${tool} setup complete!\x1b[0m`);
    console.log(`Files copied to: ${destDir}`);
}

// Discover available ClawdHub skills for interactive selection
function discoverClawdhubSkills(clawdhubDir) {
    const skills = [];

    if (!fs.existsSync(clawdhubDir)) {
        return skills;
    }

    const skillDirs = readdirSync(clawdhubDir).filter(f =>
        statSync(path.join(clawdhubDir, f)).isDirectory()
    );

    for (const skill of skillDirs) {
        const skillPath = path.join(clawdhubDir, skill);
        const skillMd = path.join(skillPath, 'SKILL.md');
        const skillJson = path.join(skillPath, 'skill.json');

        let description = skill;
        let version = 'unknown';

        // Try to get description from skill.json first
        if (fs.existsSync(skillJson)) {
            try {
                const json = JSON.parse(fs.readFileSync(skillJson, 'utf8'));
                if (json.description) description = json.description.split('.')[0]; // First sentence
                if (json.version) version = json.version;
            } catch {}
        }
        // Fallback to SKILL.md frontmatter
        else if (fs.existsSync(skillMd)) {
            const content = fs.readFileSync(skillMd, 'utf8');
            const descMatch = content.match(/description:\s*([^\n]+)/);
            if (descMatch) description = descMatch[1].trim().split('.')[0];
            const verMatch = content.match(/version:\s*["']?([^"'\n]+)/);
            if (verMatch) version = verMatch[1].trim();
        }

        skills.push({ name: skill, path: skill, description, version });
    }

    return skills;
}

async function handleClawdhubSkillsCopy(tool, config, targetFolder = null, isNonInteractive = false, specifiedSkills = null) {
    // Require target folder for ClawdHub skills
    if (!targetFolder) {
        const answers = await inquirer.prompt([
            {
                type: 'input',
                name: 'targetFolder',
                message: 'Enter the target workspace folder:',
                validate: (input) => !!input.trim() || 'Folder name required',
            },
        ]);
        targetFolder = answers.targetFolder;
    }

    const clawdhubDir = path.join(__dirname, config.ruleDir);
    const destDir = path.join(targetFolder, config.targetSubdir);
    let totalFilesCopied = 0;

    // Discover available skills
    const availableSkills = discoverClawdhubSkills(clawdhubDir);

    if (availableSkills.length === 0) {
        console.log('\\n⚠️  No ClawdHub skills found in clawdhub-skills/ directory.\\n');
        return;
    }

    let selectedSkillPaths = null;

    // If skills specified via CLI, use those
    if (specifiedSkills) {
        selectedSkillPaths = new Set(specifiedSkills);
        console.log(`\\n📦 Installing specified skills: ${specifiedSkills.join(', ')}\\n`);
    }
    // Interactive skill selection
    else if (!isNonInteractive) {
        console.log('\\n🦞 Select ClawdHub skills to install:\\n');

        const choices = availableSkills.map(skill => ({
            name: `${skill.name} (v${skill.version}) - ${skill.description}`,
            value: skill.name,
            checked: true
        }));

        const { selectedSkills } = await inquirer.prompt([
            {
                type: 'checkbox',
                name: 'selectedSkills',
                message: 'Use space to toggle, enter to confirm:',
                choices: choices,
                pageSize: 15
            }
        ]);

        if (selectedSkills.length === 0) {
            console.log('\\n⚠️  No skills selected.\\n');
            return;
        }

        selectedSkillPaths = new Set(selectedSkills);
    }
    // Non-interactive: copy all skills
    else {
        selectedSkillPaths = new Set(availableSkills.map(s => s.name));
    }

    showProgress(`Checking ${destDir} directory`);
    if (!fs.existsSync(destDir)) {
        fs.mkdirSync(destDir, { recursive: true });
        completeProgress(`Created ${destDir} directory`);
    } else {
        completeProgress(`Found ${destDir} directory`);
    }

    // Copy each selected skill
    for (const skillName of selectedSkillPaths) {
        const sourceDir = path.join(clawdhubDir, skillName);
        const targetDir = path.join(destDir, skillName);

        if (!fs.existsSync(sourceDir)) {
            console.log(`\\x1b[33m⚠\\x1b[0m  Skill not found: ${skillName}`);
            continue;
        }

        showProgress(`Copying skill: ${skillName}`);
        fs.mkdirSync(targetDir, { recursive: true });
        const filesCopied = copyDirectoryRecursive(
            sourceDir,
            targetDir,
            config.excludeFiles || [],
            config.templateSubstitutions || {}
        );
        totalFilesCopied += filesCopied;
        completeProgress(`Copied ${filesCopied} files from ${skillName}`);
    }

    console.log(`\\n\\x1b[32m🦞 ClawdHub skills installed!\\x1b[0m`);
    console.log(`Skills copied to: ${destDir} (${totalFilesCopied} files)`);
    console.log(`\\nInstalled skills:`);
    for (const skillName of selectedSkillPaths) {
        console.log(`  - ${skillName}/`);
    }
    console.log(`\\nTo publish to ClawdHub, run: clawdhub publish ${destDir}/<skill-name>`);
}

// ---------------------------------------------------------------------------
// Verify mode: read-only integrity check of installed external skills.
// Compares manifest expectations against what's actually on disk in
// ~/.{tool}/skills/ (and configured external subpath). Never mutates.
// ---------------------------------------------------------------------------
function loadManifest() {
    const manifestPath = path.join(__dirname, 'external-dependencies.yaml');
    if (!fs.existsSync(manifestPath)) return null;
    try {
        return yaml.load(fs.readFileSync(manifestPath, 'utf8'));
    } catch {
        return null;
    }
}

function hasSkillMd(dir) {
    return fs.existsSync(path.join(dir, 'SKILL.md'));
}

function findNestedSkills(root) {
    // Walk subdirs up to reasonable depth to locate any SKILL.md locations.
    // Returns array of relative paths (relative to `root`) where SKILL.md exists.
    const results = [];
    function walk(dir, rel, depth) {
        if (depth > 4) return;
        let items;
        try { items = readdirSync(dir); } catch { return; }
        for (const item of items) {
            if (item === 'node_modules' || item === '.git') continue;
            const full = path.join(dir, item);
            let st;
            try { st = statSync(full); } catch { continue; }
            if (!st.isDirectory()) continue;
            if (hasSkillMd(full)) {
                results.push(path.join(rel, item));
            }
            walk(full, path.join(rel, item), depth + 1);
        }
    }
    if (fs.existsSync(root)) walk(root, '', 0);
    return results;
}

async function handleVerify(tool, overrideHomeDir) {
    const config = TOOL_CONFIG[tool];
    if (!config) {
        console.error(`Unknown tool: ${tool}`);
        process.exit(2);
    }

    const manifest = loadManifest();
    if (!manifest) {
        console.error('Could not load external-dependencies.yaml');
        process.exit(2);
    }

    const homeDir = overrideHomeDir || os.homedir();
    const toolHome = path.join(homeDir, config.targetSubdir);
    const canonicalToolName = TOOL_CANONICAL_NAMES[tool] || tool;
    const externalSkillsPath = config.externalSkillsSubpath || 'skills';
    const externalSkillsDir = path.join(toolHome, externalSkillsPath);
    const flatSkillsDir = path.join(toolHome, 'skills');

    const RESET = '\x1b[0m';
    const GREEN = '\x1b[32m';
    const RED = '\x1b[31m';
    const YELLOW = '\x1b[33m';
    const DIM = '\x1b[2m';
    const BOLD = '\x1b[1m';
    const ok = (s) => `${GREEN}✓${RESET} ${s}`;
    const bad = (s) => `${RED}✗${RESET} ${s}`;
    const warn = (s) => `${YELLOW}⚠${RESET} ${s}`;

    console.log(`${BOLD}Verifying ${tool}${RESET} (home: ${toolHome})`);
    console.log(`${DIM}canonical-name=${canonicalToolName}  external-skills-dir=${externalSkillsDir}${RESET}\n`);

    const expectedSkillDirs = new Set();
    let present = 0;
    let missing = 0;
    let partial = 0;

    // Determine which dep types this tool configures. If the tool has no
    // externalDepTypes configured, nothing is applicable.
    const depTypes = config.externalDepTypes || [];

    // ------ agent-skills ------
    const agentSkillsApplicable = [];
    if (depTypes.includes('agent-skills') && Array.isArray(manifest['agent-skills'])) {
        for (const skill of manifest['agent-skills']) {
            if (skill['catalog-only']) continue;
            if (!skill.repo) continue;
            const appliesTo = skill['applies-to'];
            if (appliesTo && !appliesTo.includes(tool) && !appliesTo.includes(canonicalToolName)) continue;
            agentSkillsApplicable.push(skill);
        }
    }

    if (agentSkillsApplicable.length > 0) {
        console.log(`${BOLD}agent-skills${RESET} (external git repos → ${externalSkillsPath}/)`);
        for (const skill of agentSkillsApplicable) {
            if (skill['multi-subpath']) {
                // Multi-skill bundle: expect sibling dirs in externalSkillsDir.
                // Each declared sub-skill name comes from manifest `skills:` list if
                // present; otherwise we probe for any SKILL.md-bearing siblings.
                const declaredSubs = Array.isArray(skill.skills)
                    ? skill.skills.map(s => typeof s === 'string' ? s : Object.keys(s)[0])
                    : null;
                const bundleTop = path.join(externalSkillsDir, skill.name);
                // Check if sibling-flattened layout is present
                let siblingHits = [];
                let siblingMissing = [];
                if (declaredSubs) {
                    for (const sub of declaredSubs) {
                        const d = path.join(externalSkillsDir, sub);
                        expectedSkillDirs.add(sub);
                        if (hasSkillMd(d)) siblingHits.push(sub);
                        else siblingMissing.push(sub);
                    }
                } else {
                    // Fallback: scan externalSkillsDir for SKILL.md siblings produced by this bundle
                    // We can't reliably attribute siblings to bundles here; just check bundleTop.
                }
                expectedSkillDirs.add(skill.name);
                if (declaredSubs) {
                    if (siblingMissing.length === 0) {
                        console.log('  ' + ok(`${skill.name} (multi-subpath, ${siblingHits.length} sub-skills)`));
                        present++;
                    } else if (siblingHits.length > 0) {
                        console.log('  ' + warn(`${skill.name} partial: have ${siblingHits.length}/${declaredSubs.length}, missing: ${siblingMissing.join(', ')}`));
                        partial++;
                        missing++;
                    } else {
                        // Nothing flat. Check if raw nested layout exists (pre-fix state).
                        if (fs.existsSync(bundleTop)) {
                            const nested = findNestedSkills(bundleTop);
                            if (nested.length > 0) {
                                console.log('  ' + warn(`${skill.name} partial: raw-clone present, SKILL.md nested at ${nested.map(n => n).slice(0, 3).join(', ')}${nested.length > 3 ? '…' : ''} (multi-subpath flatten not applied)`));
                                partial++;
                                missing++;
                            } else {
                                console.log('  ' + bad(`${skill.name} MISSING: no sub-skills flat, no SKILL.md nested under ${bundleTop}`));
                                missing++;
                            }
                        } else {
                            console.log('  ' + bad(`${skill.name} MISSING: ${bundleTop} not found, no sub-skills flat`));
                            missing++;
                        }
                    }
                } else {
                    // No declared sub-skills — fall back to presence check at bundleTop
                    if (hasSkillMd(bundleTop)) {
                        console.log('  ' + ok(`${skill.name} (multi-subpath, root SKILL.md)`));
                        present++;
                    } else {
                        console.log('  ' + bad(`${skill.name} MISSING (no declared sub-skills, no root SKILL.md at ${bundleTop})`));
                        missing++;
                    }
                }
            } else {
                // Single-skill git clone: expect SKILL.md at externalSkillsDir/<name>/
                // For `subpath:` entries, the install copies <subpath>/. into <name>/ so
                // SKILL.md should still be at the root of <name>/.
                const skillDir = path.join(externalSkillsDir, skill.name);
                expectedSkillDirs.add(skill.name);
                if (hasSkillMd(skillDir)) {
                    console.log('  ' + ok(`${skill.name}`));
                    present++;
                } else if (fs.existsSync(skillDir)) {
                    // Present but no SKILL.md at root — likely packaging quirk
                    const nested = findNestedSkills(skillDir);
                    if (nested.length > 0) {
                        console.log('  ' + warn(`${skill.name} partial: dir exists but no root SKILL.md; nested SKILL.md at ${nested.slice(0, 3).join(', ')}${nested.length > 3 ? '…' : ''}`));
                        partial++;
                        missing++;
                    } else {
                        console.log('  ' + bad(`${skill.name} MISSING: ${skillDir} exists but has no SKILL.md anywhere`));
                        missing++;
                    }
                } else {
                    console.log('  ' + bad(`${skill.name} MISSING: ${skillDir} does not exist`));
                    missing++;
                }
            }
        }
        console.log('');
    }

    // ------ npx-skills ------
    // Expected post-mirror location: flatSkillsDir/<name>/SKILL.md
    // (Pre-mirror, they live at ~/.agents/skills/<N>/ or cwd/.agents/skills/)
    const npxApplicable = [];
    if (depTypes.includes('npx-skills') && Array.isArray(manifest['npx-skills'])) {
        for (const s of manifest['npx-skills']) {
            if (s['catalog-only']) continue;
            if (!s.install && !s.repo) continue; // skip pure-catalog entries
            npxApplicable.push(s);
        }
    }

    if (npxApplicable.length > 0) {
        console.log(`${BOLD}npx-skills${RESET} (mirrored into ${config.targetSubdir}/skills/)`);
        for (const s of npxApplicable) {
            // Collect expected sub-skill names: manifest may declare them in `skills:`
            const declaredSubs = Array.isArray(s.skills)
                ? s.skills.map(x => typeof x === 'string' ? x : Object.keys(x)[0])
                : null;
            if (declaredSubs && declaredSubs.length > 0) {
                const hits = [];
                const misses = [];
                for (const sub of declaredSubs) {
                    expectedSkillDirs.add(sub);
                    if (hasSkillMd(path.join(flatSkillsDir, sub))) hits.push(sub);
                    else misses.push(sub);
                }
                expectedSkillDirs.add(s.name);
                if (misses.length === 0) {
                    console.log('  ' + ok(`${s.name} bundle (${hits.length} sub-skills)`));
                    present++;
                } else if (hits.length > 0) {
                    console.log('  ' + warn(`${s.name} bundle partial: have ${hits.length}/${declaredSubs.length}, missing: ${misses.slice(0, 5).join(', ')}${misses.length > 5 ? '…' : ''}`));
                    partial++;
                    missing++;
                } else {
                    console.log('  ' + bad(`${s.name} bundle MISSING: none of ${declaredSubs.length} sub-skills mirrored to ${flatSkillsDir}/`));
                    missing++;
                }
            } else {
                expectedSkillDirs.add(s.name);
                if (hasSkillMd(path.join(flatSkillsDir, s.name))) {
                    console.log('  ' + ok(`${s.name}`));
                    present++;
                } else {
                    console.log('  ' + bad(`${s.name} MISSING: ${path.join(flatSkillsDir, s.name)}/SKILL.md not found`));
                    missing++;
                }
            }
        }
        console.log('');
    }

    // ------ orphan detection (warnings only) ------
    const orphans = [];
    function scanForOrphans(dir, labelPrefix) {
        if (!fs.existsSync(dir)) return;
        let entries;
        try { entries = readdirSync(dir); } catch { return; }
        for (const entry of entries) {
            const full = path.join(dir, entry);
            let st;
            try { st = statSync(full); } catch { continue; }
            if (!st.isDirectory()) continue;
            if (expectedSkillDirs.has(entry)) continue;
            if (!hasSkillMd(full)) continue; // only flag things that actually look like skills
            orphans.push(`${labelPrefix}${entry}`);
        }
    }
    scanForOrphans(externalSkillsDir, externalSkillsPath + '/');
    if (externalSkillsDir !== flatSkillsDir) {
        scanForOrphans(flatSkillsDir, 'skills/');
    }
    if (orphans.length > 0) {
        console.log(`${BOLD}orphans${RESET} (skill dirs not tracked by manifest — may be intentional)`);
        for (const o of orphans.slice(0, 25)) {
            console.log('  ' + warn(o));
        }
        if (orphans.length > 25) {
            console.log(`  ${DIM}… and ${orphans.length - 25} more${RESET}`);
        }
        console.log('');
    }

    // ------ cross-tool parity ------
    // For each manifest skill, list which of its applies-to tools have it on disk.
    console.log(`${BOLD}cross-tool parity${RESET} (${DIM}applies-to vs on-disk at default ~/.TOOL homes${RESET})`);
    function parityCheck(entries, sectionLabel, getTargetPath) {
        for (const entry of entries) {
            if (entry['catalog-only']) continue;
            const appliesTo = entry['applies-to'];
            if (!appliesTo || !Array.isArray(appliesTo)) continue;
            const hits = [];
            const miss = [];
            for (const at of appliesTo) {
                // Resolve canonical tool name back to internal config key where applicable
                let toolKey = at;
                if (at === 'claude') toolKey = 'claude-code-4.5';
                const cfg = TOOL_CONFIG[toolKey];
                if (!cfg) continue;
                const p = getTargetPath(homeDir, cfg, entry);
                if (hasSkillMd(p)) hits.push(at);
                else miss.push(at);
            }
            if (miss.length === 0) {
                console.log('  ' + ok(`${sectionLabel} ${entry.name}: all ${hits.length} tools`));
            } else if (hits.length === 0) {
                console.log('  ' + bad(`${sectionLabel} ${entry.name}: none of ${appliesTo.length} tools (expected: ${appliesTo.join(',')})`));
            } else {
                console.log('  ' + warn(`${sectionLabel} ${entry.name}: ${hits.join(',')} ✓  |  ${miss.join(',')} ✗`));
            }
        }
    }
    const agentSkillsForParity = (manifest['agent-skills'] || []).filter(s => !s['catalog-only'] && s.repo && !s['multi-subpath']);
    parityCheck(agentSkillsForParity, '[agent-skill]', (home, cfg, entry) => {
        const ext = cfg.externalSkillsSubpath || 'skills';
        return path.join(home, cfg.targetSubdir, ext, entry.name);
    });
    console.log('');

    // ------ summary ------
    const total = present + missing;
    const colourForSummary = missing === 0 ? GREEN : RED;
    console.log(`${BOLD}summary:${RESET} ${colourForSummary}${present}/${total}${RESET} skills present · ${missing} missing · ${orphans.length} orphans${partial ? ` · ${partial} partial` : ''}`);

    process.exit(missing === 0 ? 0 : 1);
}

async function main() {
    const args = process.argv.slice(2);
    const toolArg = args.find(arg => arg.startsWith('--tool='));
    const targetFolderArg = args.find(arg => arg.startsWith('--targetFolder='));
    const homeDirArg = args.find(arg => arg.startsWith('--homeDir='));
    const packagesArg = args.find(arg => arg.startsWith('--packages='));
    const selectPackagesFlag = args.includes('--selectPackages');
    const verifyFlag = args.includes('--verify');
    const sddShortcut = args.includes('--sdd');
    // If --selectPackages is passed, we want interactive package selection
    const isNonInteractive = (!!toolArg || sddShortcut) && !selectPackagesFlag;

    let tool = toolArg ? toolArg.split('=')[1] : null;
    let targetFolder = targetFolderArg ? targetFolderArg.split('=')[1] : null;
    let overrideHomeDir = homeDirArg ? homeDirArg.split('=')[1] : null;

    // Parse --packages=skills/webapp-testing,agents/engineering,...
    let specifiedPackages = null;
    if (packagesArg) {
        specifiedPackages = packagesArg.split('=')[1].split(',').map(p => p.trim());
    }

    // Convenience: allow --sdd without specifying a tool
    if (!tool && sddShortcut) {
        tool = 'sdd';
    }

    if (!tool) {
        const answers = await inquirer.prompt([
            {
                type: 'list',
                name: 'tool',
                message: 'Select the tool:',
                choices: [...Object.keys(TOOL_CONFIG), 'sdd'],
            },
        ]);
        tool = answers.tool;
    }

    // Verify mode: read-only integrity check, never mutates filesystem.
    if (verifyFlag) {
        await handleVerify(tool, overrideHomeDir);
        return;
    }

    // Handle SDD-only installation mode (no rules), copying assets from spec-kit into a project
    if (tool === 'sdd') {
        // Single, simple path: clone repo then copy assets
        const repo = process.env.SPEC_KIT_REPO || 'https://github.com/github/spec-kit.git';
        const ref = process.env.SPEC_KIT_REF || '';
        // pick destination
        const dest = targetFolder || (await inquirer.prompt([{ type: 'input', name: 'sddDest', message: 'Project folder for Spec Kit (SDD) assets:', validate: (v) => !!v.trim() || 'Folder name required' }])).sddDest;
        const clonePath = await cloneSpecKit(repo, ref);
        await copySpecKitAssets(clonePath, dest);
        return;
    }

    const config = TOOL_CONFIG[tool];

    // Handle ClawdHub skills (copy to target workspace/skills/)
    if (config.useClawdhubStructure) {
        await handleClawdhubSkillsCopy(tool, config, targetFolder, isNonInteractive, specifiedPackages);
        return;
    }

    // Handle packages structure (new catalog-based structure)
    if (config.usePackagesStructure) {
        await handlePackagesStructureCopy(tool, config, overrideHomeDir, targetFolder, isNonInteractive, specifiedPackages);
        return;
    }

    if (config.copyEntireFolder) {
        await handleFullDirectoryCopy(tool, config, overrideHomeDir, targetFolder);
        return;
    }

    if (config.copySharedContent) {
        await handleSharedContentCopy(tool, config, targetFolder);
        return;
    }

    if (!targetFolder) {
        const answers = await inquirer.prompt([
            {
                type: 'input',
                name: 'targetFolder',
                message: 'Enter the target project folder:',
                validate: (input) => !!input.trim() || 'Folder name required',
            },
        ]);
        targetFolder = answers.targetFolder;
    }
    
    showProgress('Creating target directory');
    if (!fs.existsSync(targetFolder)) {
        fs.mkdirSync(targetFolder, { recursive: true });
        completeProgress(`Created folder: ${targetFolder}`);
    } else {
        completeProgress(`Using existing folder: ${targetFolder}`);
    }

    showProgress('Copying tool-specific rules');
    const destDir = path.join(targetFolder, config.targetSubdir);
    fs.mkdirSync(destDir, { recursive: true });
    
    const rulePath = path.join(__dirname, config.ruleDir, config.ruleGlob);
    fs.copyFileSync(rulePath, path.join(destDir, config.ruleGlob));
    
    for (const rule of ALWAYS_COPY_RULES) {
        fs.copyFileSync(path.join(GENERAL_RULES_DIR, rule), path.join(destDir, rule));
    }
    completeProgress('Copied core rules');

    const copiedFiles = [config.ruleGlob, ...ALWAYS_COPY_RULES];
    const generalRuleFiles = getGeneralRuleFiles();
    if (!isNonInteractive && generalRuleFiles.length > 0) {
        const { selectedGeneralRules } = await inquirer.prompt([
            {
                type: 'checkbox',
                name: 'selectedGeneralRules',
                message: 'Select additional general rules to copy:',
                choices: generalRuleFiles,
            },
        ]);
        
        if (selectedGeneralRules && selectedGeneralRules.length > 0) {
            showProgress('Copying additional rules');
            for (const ruleFile of selectedGeneralRules) {
                fs.copyFileSync(path.join(GENERAL_RULES_DIR, ruleFile), path.join(destDir, ruleFile));
                copiedFiles.push(ruleFile);
            }
            completeProgress(`Copied ${selectedGeneralRules.length} additional rules`);
        }
    }

    // Copy shared content to specific target if specified
    if (config.sharedContentTarget) {
        const targetDir = path.join(targetFolder, config.sharedContentTarget);
        showProgress(`Copying shared content to ${config.sharedContentTarget}`);
        const sharedSourceDir = path.join(__dirname, config.sharedContentDir);
        fs.mkdirSync(targetDir, { recursive: true });
        const excludeFiles = getEffectiveExcludeFiles(tool, config);
        const sharedFilesCopied = copyDirectoryRecursive(sharedSourceDir, targetDir, excludeFiles, config.templateSubstitutions || {});
        completeProgress(`Copied ${sharedFilesCopied} shared files to ${config.sharedContentTarget}`);
    }

    // Copy MCP file to specified target if specified
    if (config.mcpFile && config.mcpTarget) {
        showProgress('Copying MCP configuration');
        const sourcePath = path.join(__dirname, config.mcpFile);
        const destPath = path.join(targetFolder, config.mcpTarget);
        
        if (fs.existsSync(sourcePath)) {
            fs.mkdirSync(path.dirname(destPath), { recursive: true });
            fs.copyFileSync(sourcePath, destPath);
            completeProgress(`Copied MCP config to ${config.mcpTarget}`);
        }
    }

    // Perform special file copies
    if (config.specialCopies) {
        showProgress('Performing special file copies');
        let specialFilesCopied = 0;
        for (const copy of config.specialCopies) {
            const sourcePath = path.join(__dirname, copy.source);
            const destPath = path.join(targetFolder, copy.dest);
            const fileName = path.basename(copy.source);

            if (fs.existsSync(sourcePath)) {
                fs.mkdirSync(path.dirname(destPath), { recursive: true });

                if (config.templateSubstitutions && config.templateSubstitutions[fileName]) {
                    let content = fs.readFileSync(sourcePath, 'utf8');
                    content = substituteTemplate(content, config.templateSubstitutions[fileName]);
                    fs.writeFileSync(destPath, content);
                } else {
                    fs.copyFileSync(sourcePath, destPath);
                }
                specialFilesCopied++;
            }
        }
        completeProgress(`Copied ${specialFilesCopied} special files`);
    }

    

    showProgress('Generating rule registry');
    const registry = {};
    for (const file of copiedFiles) {
        const filePath = path.join(destDir, file);
        const front = parseFrontMatter(filePath);
        if (front) {
            registry[file.replace(/\..*$/, '')] = {
                path: path.join(config.targetSubdir, file),
                globs: Array.isArray(front.globs) ? front.globs : (front.globs ? [front.globs] : []),
                alwaysApply: !!front.alwaysApply
            };
        }
    }
    fs.writeFileSync(path.join(destDir, 'rule-registry.json'), JSON.stringify(registry, null, 4));
    completeProgress('Generated rule registry');

    console.log('\n\x1b[32m🎉 Setup complete!\x1b[0m');
    console.log(`Files copied to: ${destDir}`);
}

main().catch((err) => {
    console.error(err);
    process.exit(1);
});

// --- Spec Kit integration: clone-on-demand, then copy ---
import { execSync } from 'child_process';

async function cloneSpecKit(repoUrl, ref = '') {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), 'spec-kit-'));
    const cloneDir = path.join(base, 'src');
    showProgress(`Cloning Spec Kit from ${repoUrl}`);
    execSync(`git clone --depth 1 ${repoUrl} ${cloneDir}`, { stdio: 'pipe' });
    if (ref && ref.trim()) {
        execSync(`git -C ${cloneDir} fetch origin ${ref} --depth 1`, { stdio: 'pipe' });
        execSync(`git -C ${cloneDir} checkout ${ref}`, { stdio: 'pipe' });
    }
    completeProgress('Cloned Spec Kit');
    return cloneDir;
}
async function copySpecKitAssets(specKitRoot, projectRoot) {
    const requiredFiles = {
        claudeCommands: [
            { source: '.claude/commands/specify.md', dest: '.claude/commands/sdd-specify.md' },
            { source: '.claude/commands/plan.md', dest: '.claude/commands/sdd-plan.md' },
            { source: '.claude/commands/tasks.md', dest: '.claude/commands/sdd-tasks.md' },
        ],
        scripts: [
            'scripts/create-new-feature.sh',
            'scripts/setup-plan.sh',
            'scripts/check-task-prerequisites.sh',
            'scripts/common.sh',
            'scripts/get-feature-paths.sh',
            'scripts/update-agent-context.sh',
        ],
        templates: [
            'templates/spec-template.md',
            'templates/plan-template.md',
            'templates/tasks-template.md',
            'templates/agent-file-template.md',
        ],
        memory: [
            'memory/constitution.md',
        ],
    };

    const exists = (p) => fs.existsSync(p);
    const src = (rel) => path.join(specKitRoot, rel);
    const dst = (rel) => path.join(projectRoot, rel);

    if (!exists(specKitRoot)) {
        throw new Error(`[SDD] Spec Kit path not found: ${specKitRoot}`);
    }

    console.log(`\n⠋ Installing Spec Kit (SDD) assets from: ${specKitRoot}`);

    // Ensure directories
    const ensureParent = (p) => fs.mkdirSync(path.dirname(p), { recursive: true });
    const copyFileIdempotent = (from, to, makeExecutable = false) => {
        ensureParent(to);
        if (exists(to)) {
            const a = fs.readFileSync(from);
            const b = fs.readFileSync(to);
            if (Buffer.compare(a, b) === 0) {
                return 'skipped';
            }
            fs.copyFileSync(to, `${to}.bak`);
        }
        fs.copyFileSync(from, to);
        if (makeExecutable) {
            try { fs.chmodSync(to, 0o755); } catch {}
        }
        return 'copied';
    };

    // Commands (with renaming)
    for (const cmd of requiredFiles.claudeCommands) {
        const sourcePath = cmd.source;
        const destPath = cmd.dest;
        const from = src(`.${path.sep}${sourcePath.split('/').slice(1).join(path.sep)}`); // map .claude/commands under spec-kit root
        const to = dst(destPath);
        if (!exists(from)) {
            // In spec-kit, commands live at .claude/commands
            const alt = src(sourcePath);
            copyFileIdempotent(exists(alt) ? alt : from, to);
        } else {
            copyFileIdempotent(from, to);
        }
    }

    // Scripts (executable)
    for (const rel of requiredFiles.scripts) {
        const from = src(rel);
        const to = dst(rel);
        copyFileIdempotent(from, to, true);
    }

    // Templates
    for (const rel of requiredFiles.templates) {
        const from = src(rel);
        const to = dst(rel);
        copyFileIdempotent(from, to);
    }

    // Memory
    for (const rel of requiredFiles.memory) {
        const from = src(rel);
        const to = dst(rel);
        copyFileIdempotent(from, to);
    }

    // Optional quickstart doc (generated)
    const quickstartPath = dst('docs/sdd-quickstart.md');
    ensureParent(quickstartPath);
    if (!exists(quickstartPath)) {
        const qs = `# Spec-Driven Development (SDD) Quickstart\n\n- Requires: Git, bash (macOS/Linux/WSL).\n- Commands live in \`.claude/commands\` for Claude Code.\n\nWorkflow:\n- /sdd-specify → creates branch + \`specs/###-.../spec.md\`\n- /sdd-plan → fills \`plan.md\` and generates research/data model/contracts/quickstart\n- /sdd-tasks → creates \`tasks.md\` from available docs\n\nRun manually (if needed):\n- \`bash scripts/create-new-feature.sh --json \"My feature\"\`\n- \`bash scripts/setup-plan.sh --json\` (must be on feature branch)\n- \`bash scripts/check-task-prerequisites.sh --json\`\n\nBranch rule: must match \`^[0-9]{3}-\` for /sdd-plan and /sdd-tasks.\n`;
        fs.writeFileSync(quickstartPath, qs);
    }

    console.log('\x1b[32m✓ Installed Spec Kit assets\x1b[0m');
}
