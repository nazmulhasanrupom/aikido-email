-- A test email goes out at once, outside the queue. This marks the campaign checklist item.
ALTER TABLE campaigns ADD COLUMN test_sent_at timestamptz;
