-- The Optimised mode keeps its own prompts, so they never mix with the AI mode prompts.
ALTER TABLE sequences
    ADD COLUMN opt_system_prompt text CHECK (opt_system_prompt IS NULL OR char_length(opt_system_prompt) <= 8000),
    ADD COLUMN opt_user_prompt   text CHECK (opt_user_prompt IS NULL OR char_length(opt_user_prompt) <= 8000);
