-- Second LLM provider. Anthropic stays the default so an existing key
-- keeps working; the dashboard chooses which one is called.
ALTER TABLE settings ADD COLUMN openai_api_key TEXT NOT NULL DEFAULT '';
ALTER TABLE settings ADD COLUMN llm_provider TEXT NOT NULL DEFAULT 'anthropic';
ALTER TABLE settings ADD COLUMN openai_model TEXT NOT NULL DEFAULT 'gpt-6.1-sol';
