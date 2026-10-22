import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { z } from "zod";
import { authStateQuery, refreshAuthState, setup } from "@/api/auth";
import { ApiProblem, messageOf } from "@/api/errors";
import { AuthLayout } from "@/components/AuthLayout";
import { Button } from "@/components/Button";
import { Field } from "@/components/Field";
import { PasswordField } from "@/components/PasswordField";
import { StrengthMeter } from "@/components/StrengthMeter";
import { HOME } from "@/lib/next";

export const Route = createFileRoute("/setup")({
  beforeLoad: async ({ context }) => {
    const state = await context.queryClient.ensureQueryData(authStateQuery);
    if (state.logged_in) throw redirect({ href: HOME });
    if (!state.setup_needed) throw redirect({ to: "/login", search: {} });
  },
  component: SetupPage,
});

const schema = z.object({
  name: z.string().trim().min(1, "Type your name.").max(100, "Use up to 100 characters."),
  email: z
    .string()
    .trim()
    .min(1, "Type your email.")
    .email("This email address does not look right."),
  password: z
    .string()
    .min(12, "Use at least 12 characters.")
    .max(128, "Use at most 128 characters."),
  workspace_name: z
    .string()
    .trim()
    .min(1, "Give the workspace a name.")
    .max(80, "Use up to 80 characters."),
});
type Values = z.infer<typeof schema>;

/** Which field a server error belongs to. */
const FIELD_OF: Record<string, keyof Values> = {
  invalid_name: "name",
  invalid_email: "email",
  invalid_workspace_name: "workspace_name",
  password_too_short: "password",
  password_too_long: "password",
  password_too_weak: "password",
  password_has_personal_info: "password",
};

function SetupPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [problem, setProblem] = useState<string>();
  const {
    register,
    handleSubmit,
    setError,
    watch,
    formState: { errors, isSubmitting },
  } = useForm<Values>({ resolver: zodResolver(schema) });
  const [password, name, email] = watch(["password", "name", "email"]);

  async function onSubmit(values: Values) {
    setProblem(undefined);
    try {
      await setup(values);
      await refreshAuthState(queryClient);
      await navigate({ to: "/onboarding/$step", params: { step: "design" } });
    } catch (e) {
      const field = e instanceof ApiProblem ? FIELD_OF[e.code] : undefined;
      if (field) setError(field, { message: messageOf(e) });
      else setProblem(messageOf(e));
    }
  }

  return (
    <AuthLayout title="Set up AIKIDO EMAIL" intro="Make the owner account. The first account becomes the owner.">
      <form onSubmit={handleSubmit(onSubmit)} noValidate className="flex flex-col gap-4">
        <Field
          label="Your name"
          autoComplete="name"
          autoFocus
          error={errors.name?.message}
          {...register("name")}
        />
        <Field
          label="Email"
          type="email"
          autoComplete="username"
          error={errors.email?.message}
          {...register("email")}
        />
        <div className="flex flex-col gap-2">
          <PasswordField
            label="Password"
            autoComplete="new-password"
            help="At least 12 characters. A long phrase of unrelated words is best."
            error={errors.password?.message}
            {...register("password")}
          />
          <StrengthMeter password={password ?? ""} inputs={[name ?? "", email ?? ""]} />
        </div>
        <Field
          label="Workspace name"
          error={errors.workspace_name?.message}
          {...register("workspace_name")}
        />
        {problem && (
          <p role="alert" className="text-sm text-danger">
            {problem}
          </p>
        )}
        <Button type="submit" busy={isSubmitting}>
          Create account
        </Button>
      </form>
    </AuthLayout>
  );
}
