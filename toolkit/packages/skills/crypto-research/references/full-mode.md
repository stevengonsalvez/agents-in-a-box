---
allowed-tools: Bash(date:*), Bash(mkdir:*), Task, Write
argument-hint: [crypto_ticker_symbol]
description: Execute comprehensive cryptocurrency research using all crypto agents
---

# Crypto Research Command

Think hard and execute comprehensive cryptocurrency research by calling all crypto analysis agents in parallel.

## Variables

- **TICKER**: $ARGUMENTS or "BTC" if not specified
  - The cryptocurrency ticker symbol to analyze (e.g., BTC, ETH, SOL)
  - Used by: crypto-coin-analyzer agents

## Agent Groups

### Market Data Agents
- @agent-crypto-market-agent-haiku
- @agent-crypto-market-agent-sonnet
- @agent-crypto-market-agent-opus

### Coin Analysis Agents
- @agent-crypto-coin-analyzer-haiku (analyze TICKER)
- @agent-crypto-coin-analyzer-sonnet (analyze TICKER)
- @agent-crypto-coin-analyzer-opus (analyze TICKER)

### Macro Correlation Agents
- @agent-macro-crypto-correlation-scanner-haiku
- @agent-macro-crypto-correlation-scanner-sonnet
- @agent-macro-crypto-correlation-scanner-opus

### Investment Plays Agents
- @agent-crypto-investment-plays-haiku
- @agent-crypto-investment-plays-sonnet
- @agent-crypto-investment-plays-opus

## Execution Instructions

1. Run `date +"%Y-%m-%d_%H-%M-%S"` to get a human-readable timestamp (e.g., 2025-01-08_14-30-45)
2. Create base output directory: `outputs/<timestamp>/`
3. Set the TICKER variable to the desired cryptocurrency (e.g., BTC, ETH, SOL)
4. Call all 12 agents listed above in parallel
5. Coin analyzer agents (crypto-coin-analyzer-haiku, crypto-coin-analyzer-sonnet, crypto-coin-analyzer-opus) will receive the TICKER parameter for focused analysis
6. Each agent should execute their respective analysis based on their specialized prompts
7. IMPORTANT: Write the complete, unmodified output from each agent to its designated file
8. Write outputs to organized directory structure:
   - `outputs/<timestamp>/crypto_market/<agent-name>.md` for market agents
   - `outputs/<timestamp>/crypto_analysis/<agent-name>.md` for coin analyzers
   - `outputs/<timestamp>/crypto_macro/<agent-name>.md` for macro correlation scanners
   - `outputs/<timestamp>/crypto_plays/<agent-name>.md` for investment plays agents

## Output Format

IMPORTANT: Write each agent's complete response directly to its respective file with NO modifications, NO summarization, and NO changes to the output whatsoever. The exact response from each agent must be preserved.

Each agent's output should be written to its respective file with no modifications. The directory structure ensures organized research results:

```
outputs/
└── 2025-01-08_14-30-45/    # Example timestamp
    ├── crypto_market/
    │   ├── crypto-market-agent-haiku.md
    │   ├── crypto-market-agent-sonnet.md
    │   └── crypto-market-agent-opus.md
    ├── crypto_analysis/
    │   ├── crypto-coin-analyzer-haiku.md
    │   ├── crypto-coin-analyzer-sonnet.md
    │   └── crypto-coin-analyzer-opus.md
    ├── crypto_macro/
    │   ├── macro-crypto-correlation-scanner-haiku.md
    │   ├── macro-crypto-correlation-scanner-sonnet.md
    │   └── macro-crypto-correlation-scanner-opus.md
    └── crypto_plays/
        ├── crypto-investment-plays-haiku.md
        ├── crypto-investment-plays-sonnet.md
        └── crypto-investment-plays-opus.md
```

## Report
When all agents are complete: give the path to the outputs/<timestamp>/ directory with the number of successful/total agents based on the existence of their respective files.
