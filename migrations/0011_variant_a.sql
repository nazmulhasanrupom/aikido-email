-- UI update map v2, I6: variant A (step 1 itself) can be turned off, so the weaker of two variants can always be turned off.
ALTER TABLE sequences ADD COLUMN variant_a_active boolean NOT NULL DEFAULT true;
