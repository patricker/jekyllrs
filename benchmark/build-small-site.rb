#!/usr/bin/env ruby
# frozen_string_literal: true

require 'fileutils'
require 'benchmark'
require_relative '../lib/jekyll'

# Minimal end-to-end build benchmark exercising Rust engine and Liquid filters.
# Usage: ruby benchmark/build-small-site.rb [COUNT]

COUNT = (ARGV[0] || ENV['COUNT'] || '100').to_i
ROOT  = File.expand_path('..', __dir__)
SRC   = File.join(ROOT, 'tmp', 'bench_site', 'src')
DST   = File.join(ROOT, 'tmp', 'bench_site', 'dest')

FileUtils.rm_rf(File.dirname(SRC))
FileUtils.mkdir_p(SRC)
FileUtils.mkdir_p(DST)

# Layouts
FileUtils.mkdir_p(File.join(SRC, '_layouts'))
File.write(File.join(SRC, '_layouts', 'default.html'), <<~HTML)
  <!doctype html>
  <html><head><meta charset="utf-8"><title>{{ page.title }}</title></head>
  <body>
    <main>
      {{ content }}
      <div id="listing">{% assign titles = site.posts | sort: 'title', 'last' | map: 'title' %}{{ titles | join: ', ' }}</div>
    </main>
  </body></html>
HTML

# Config
File.write(File.join(SRC, '_config.yml'), <<~YML)
  title: Bench Site
  baseurl: ''
  markdown: kramdown
YML

# Posts
FileUtils.mkdir_p(File.join(SRC, '_posts'))
now = Time.now
COUNT.times do |i|
  date = (now - i * 86400).strftime('%Y-%m-%d')
  title = (i % 10 == 0) ? '' : "Post #{i}"
  content = (i % 7 == 0) ? "{% include_relative noop.md %}" : "Hello from #{i}!"
  File.write(File.join(SRC, '_posts', "#{date}-post-#{i}.md"), <<~MD)
    ---
    layout: default
    title: #{title}
    ---
    #{content}
  MD
end

# Include folder with a no-op include
FileUtils.mkdir_p(File.join(SRC, '_includes'))
File.write(File.join(SRC, '_includes', 'noop.md'), "noop\n")

# Index page
File.write(File.join(SRC, 'index.md'), <<~MD)
  ---
  layout: default
  title: Home
  ---
  Welcome!
MD

config = Jekyll::Configuration.from(
  'source' => SRC,
  'destination' => DST,
  'incremental' => false,
  'profile' => false,
)

site = Jekyll::Site.new(config)
GC.start
elapsed = Benchmark.realtime { site.process }

puts "Built #{COUNT + 1} documents in #{format('%.3f', elapsed)}s"
