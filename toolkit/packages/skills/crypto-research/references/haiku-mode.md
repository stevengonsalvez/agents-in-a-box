---
allowed-tools: Bash(date:*), Bash(mkdir:*), Task, Write
argument-hint: [crypto_ticker_symbol]
description: Execute lightweight cryptocurrency research using haiku agents
---

# Crypto Research Haiku Command

Execute lightweight cryptocurrency research by calling only haiku versions of crypto analysis agents in parallel.

## Variables

- **TICKER**: $ARGUMENTS
  - The cryptocurrency ticker symbol to analyze (e.g., BTC, ETH, SOL)
  - Used by: crypto-coin-analyzer-haiku agent

## Agents

### All Haiku Agents
- @agent-crypto-market-agent-haiku
- @agent-crypto-coin-analyzer-haiku (analyze TICKER)
- @agent-macro-crypto-correlation-scanner-haiku
- @agent-crypto-investment-plays-haiku

## Execution Instructions

1. Run `date +"%Y-%m-%d_%H-%M-%S"` to get a human-readable timestamp (e.g., 2025-01-08_14-30-45)
2. Create base output directory: `outputs/<timestamp>/haiku/`
3. Set the TICKER variable to the desired cryptocurrency (e.g., BTC, ETH, SOL)
4. Call all 4 haiku agents listed above in parallel
5. Coin analyzer agent (crypto-coin-analyzer-haiku) will receive the TICKER parameter for focused analysis
6. Each agent should execute their respective analysis based on their specialized prompts
7. IMPORTANT: Write the complete, unmodified output from each agent to its designated file
8. Write outputs to organized directory structure:
   - `outputs/<timestamp>/haiku/crypto_market/crypto-market-agent-haiku.md` for market agent
   - `outputs/<timestamp>/haiku/crypto_analysis/crypto-coin-analyzer-haiku.md` for coin analyzer
   - `outputs/<timestamp>/haiku/crypto_macro/macro-crypto-correlation-scanner-haiku.md` for macro correlation scanner
   - `outputs/<timestamp>/haiku/crypto_plays/crypto-investment-plays-haiku.md` for investment plays agent

## Output Format

IMPORTANT: Write each agent's complete response directly to its respective file with NO modifications, NO summarization, and NO changes to the output whatsoever. The exact response from each agent must be preserved.

Each agent's output should be written to its respective file with no modifications. The directory structure ensures organized research results:

```
outputs/
└── 2025-01-08_14-30-45/    # Example timestamp
    └── haiku/
        ├── crypto_market/
        │   └── crypto-market-agent-haiku.md
        ├── crypto_analysis/
        │   └── crypto-coin-analyzer-haiku.md
        ├── crypto_macro/
        │   └── macro-crypto-correlation-scanner-haiku.md
        └── crypto_plays/
            └── crypto-investment-plays-haiku.md
```

## Report
When all agents are complete: give the path to the outputs/<timestamp>/haiku/ directory with the number of successful/total agents based on the existence of their respective files.
