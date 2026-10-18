-- PERSONALIZATION v2: the AI writes short points about the business from a link and a file. Prompts use them as {{my_business}}.
ALTER TABLE sender_profiles ADD COLUMN my_business text NOT NULL DEFAULT '';
