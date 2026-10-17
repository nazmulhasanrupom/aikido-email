import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { z } from "zod";
import { authStateQuery, login, refreshAuthState } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { AuthLayout } from "@/components/AuthLayout";
import { Button } from "@/components/Button";
import { Field } from "@/components/Field";
import { PasswordField } from "@/components/PasswordField";
import { HOME, safeNext } from "@/lib/next";

export const Route = createFileRoute("/login")({
  validateSearch: (search: Record<string, unknown>): { next?: string } => {
    const next = safeNext(search.next);
    return next ? { next } : {};
  },
  beforeLoad: async ({ context }) => {
    const state = await context.queryClient.ensureQueryData(authStateQuery);
    if (state.setup_needed) throw redirect({ to: "/setup" });
    if (state.logged_in) throw redirect({ href: HOME });
  },
  component: LoginPage,
});

const schema = z.object({
  email: z.string().trim().min(1, "Type your email."),
  password: z.string().min(1, "Type your password."),
});
type Values = z.infer<typeof schema>;

function LoginPage() {
  const { next } = Route.useSearch();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [problem, setProblem] = useState<string>();
  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
  } = useForm<Values>({ resolver: zodResolver(schema) });

  async function onSubmit(values: Values) {
    setProblem(undefined);
    try {
      await login(values.email, values.password);
      await refreshAuthState(queryClient);
      await navigate({ href: next ?? HOME });
    } catch (e) {
      setProblem(messageOf(e));
    }
  }

  return (
    <AuthLayout title="Log in">
      <form onSubmit={handleSubmit(onSubmit)} noValidate className="flex flex-col gap-4">
        <Field
          label="Email"
          type="email"
          autoComplete="username"
          autoFocus
          error={errors.email?.message}
          {...register("email")}
        />
        <PasswordField
          label="Password"
          autoComplete="current-password"
          error={errors.password?.message}
          {...register("password")}
        />
        {problem && (
          <p role="alert" className="text-sm text-danger">
            {problem}
          </p>
        )}
        <Button type="submit" busy={isSubmitting}>
          Log in
        </Button>
        <p className="text-center text-xs text-muted">
          Forgot your password? Ask an admin to reset it.
        </p>
      </form>
    </AuthLayout>
  );
}
