# frozen_string_literal: true

require "helper"
require "mercenary"
require "jekyll/cli/serve_command"

class TestCommandsServe < JekyllUnitTest
  def setup
    super
    @program = nil
    Mercenary.program(:jekyll) do |p|
      @program = p
      Jekyll::CLI::ServeCommand.register(p)
    end
    @command = @program.commands[:serve]
    @old_env = ENV["JEKYLL_ENV"]
    ENV["JEKYLL_ENV"] = "development"
  end

  def teardown
    ENV["JEKYLL_ENV"] = @old_env
    super
  end

  should "enable watch by default and assign development url" do
    captured = nil
    allow(Jekyll::Rust).to receive(:engine_build_process) do |config|
      captured = config
    end
    allow(Jekyll::Rust).to receive(:engine_serve_process)

    execute_serve

    refute_nil captured, "expected engine_build_process to be invoked"
    assert_equal true, captured["watch"], "watch should default to true"
    assert_equal "http://localhost:4000", captured["url"], "expected default url in development"
  end

  should "force watch and disable detach when livereload is enabled" do
    captured = nil
    allow(Jekyll::Rust).to receive(:engine_build_process)
    allow(Jekyll::Rust).to receive(:engine_serve_process) do |config|
      captured = config
    end

    execute_serve(
      "livereload" => true,
      "detach" => true,
      "livereload_ignore" => ["*.tmp"],
    )

    refute_nil captured
    assert_equal true, captured["watch"], "livereload should enable watch"
    assert_equal false, captured["detach"], "detach must be disabled when livereload is on"
    assert_equal Jekyll::CLI::ServeCommand::DEFAULT_LIVERELOAD_PORT, captured["livereload_port"]
  end

  should "abort when livereload-specific flags are used without livereload" do
    allow(Jekyll::Rust).to receive(:engine_build_process)
    allow(Jekyll::Rust).to receive(:engine_serve_process)

    assert_raises(SystemExit) do
      execute_serve("livereload_min_delay" => 2)
    end
  end

  private

  def execute_serve(options = {})
    raise "serve command was not initialized" unless @command

    # Mercenary mutates the options hash, so operate on a dup to keep assertions predictable.
    @command.execute([], options.dup)
  end
end
